//! Persistence in the `nvs` data partition (PROJECT_BRIEF.md §7.8).
//!
//! Two independent records live in two flash sectors so they can be managed separately:
//! WiFi credentials (sector 0, cleared by the BOOT-button reset, UC3) and the connection
//! selection (sector 1). Each record is `[magic u32][len u32][JSON payload]`; a wrong
//! magic means "empty". We use `serde-json-core` for the payload so the format is obvious
//! and forward-compatible.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use esp_bootloader_esp_idf::partitions::{
    self, DataPartitionSubType, PartitionType, PARTITION_TABLE_MAX_LEN,
};
use esp_hal::peripherals::FLASH;
use esp_storage::FlashStorage;
use log::{info, warn};
use serde::{Deserialize, Serialize};

use crate::model::{Selection, WifiCreds};

const SECTOR: u32 = FlashStorage::SECTOR_SIZE; // 4096
// Both records live in ONE sector at the start of the nvs partition. An earlier design put
// the selection in a second sector (offset 4096), but writes there reported success yet read
// back empty after a reboot (the offset landed outside the usable region). Sector 0 is proven
// to round-trip (WiFi survived reboots), so we keep everything here and read-modify-write.
const STORE_SECTOR: u32 = 0;
const MAGIC: u32 = 0x5A47_4C32; // "ZGL2" — bumped; old single-record format is ignored
const MAX_PAYLOAD: usize = 768;

/// Everything persisted, as one record so both fields survive independent updates.
#[derive(Default, Serialize, Deserialize)]
struct Persisted {
    wifi: Option<WifiCreds>,
    selection: Option<Selection>,
}

/// Globally shared flash store, initialised once at boot via [`init`].
pub static STORE: Mutex<CriticalSectionRawMutex, Option<Store>> = Mutex::new(None);

/// Owns the flash peripheral and the resolved `nvs` partition offset.
pub struct Store {
    flash: FlashStorage<'static>,
    nvs_offset: u32,
}

impl Store {
    /// Resolve the `nvs` partition from the partition table and take ownership of flash.
    pub fn new(flash_periph: FLASH<'static>) -> Result<Self, ()> {
        // The display render loop runs on the second core. esp-storage's default policy is
        // to refuse a flash write while the other core is live (it would fault executing
        // from flash with the cache disabled). `multicore_auto_park` parks that core for the
        // few-ms write and un-parks it after — without it every save returns OtherCoreRunning.
        let mut flash = FlashStorage::new(flash_periph).multicore_auto_park();
        let mut table_buf = [0u8; PARTITION_TABLE_MAX_LEN];
        let table = partitions::read_partition_table(&mut flash, &mut table_buf).map_err(|_| ())?;
        let nvs = table
            .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))
            .map_err(|_| ())?
            .ok_or(())?;
        let nvs_offset = nvs.offset();
        info!(
            "storage: nvs partition at {:#x}, size {:#x}",
            nvs_offset,
            nvs.len()
        );

        // DIAGNOSTIC (temporary): raw dump of the store sector, read directly from flash
        // before any magic/deserialize logic. Tells us whether persisted bytes physically
        // survive a reboot, independent of how we decode them. Bytes 0..4 are the magic (LE),
        // 4..8 the payload length, 8.. the JSON payload. All-`ff` means an erased sector.
        let base = nvs_offset + STORE_SECTOR;
        let mut head = [0u8; 8 + MAX_PAYLOAD];
        if flash.read(base, &mut head).is_ok() {
            let len = u32::from_le_bytes(head[4..8].try_into().unwrap()) as usize;
            info!("storage: RAW @{base:#x} hdr = {:02x?}", &head[..8]);
            if (1..=MAX_PAYLOAD).contains(&len) {
                let payload = &head[8..8 + len];
                info!("storage: RAW payload bytes = {payload:02x?}");
                match core::str::from_utf8(payload) {
                    Ok(s) => info!("storage: RAW payload text  = {s}"),
                    Err(_) => warn!("storage: RAW payload is not valid UTF-8"),
                }
            }
        } else {
            warn!("storage: RAW @{base:#x} read failed");
        }

        Ok(Self { flash, nvs_offset })
    }

    fn read_record<'a, T: serde::Deserialize<'a>>(
        &mut self,
        sector: u32,
        magic: u32,
        scratch: &'a mut [u8; MAX_PAYLOAD],
    ) -> Option<T> {
        let base = self.nvs_offset + sector;
        let mut header = [0u8; 8];
        if self.flash.read(base, &mut header).is_err() {
            warn!("storage: READ @{base:#x} flash read failed");
            return None;
        }
        let found_magic = u32::from_le_bytes(header[0..4].try_into().unwrap());
        let len = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        info!(
            "storage: READ @{base:#x} found_magic={found_magic:#x} (want {magic:#x}) len={len}"
        );
        if found_magic != magic {
            return None;
        }
        if len == 0 || len > MAX_PAYLOAD {
            return None;
        }
        // esp-storage's `ReadNorFlash::read` requires a 4-byte-aligned offset AND length
        // (`READ_SIZE == WORD_SIZE == 4`); an unaligned length returns `NotAligned`. `len`
        // (e.g. 78) usually isn't a multiple of 4, so read up to the next word boundary —
        // the writer already padded the record to that boundary, so the extra bytes are
        // valid flash content. We still deserialize exactly `len` bytes below. (Without
        // this, the failed read became a silent `None`, so creds read back as absent.)
        let read_len = len.div_ceil(4) * 4;
        if self.flash.read(base + 8, &mut scratch[..read_len]).is_err() {
            warn!("storage: READ @{base:#x} payload read failed");
            return None;
        }
        // Decode any `\uXXXX` escapes (e.g. `ü`) so the loaded value matches what was saved;
        // plain `from_slice` would leave them literal. Buffer fits the longest field value.
        let mut unescape = [0u8; 96];
        match serde_json_core::from_slice_escaped::<T>(&scratch[..len], &mut unescape) {
            Ok((v, _)) => Some(v),
            Err(e) => {
                warn!("storage: READ @{base:#x} deserialize failed: {e:?}");
                None
            }
        }
    }

    fn write_record<T: serde::Serialize>(
        &mut self,
        sector: u32,
        magic: u32,
        value: &T,
    ) -> Result<(), ()> {
        let mut payload = [0u8; MAX_PAYLOAD];
        let len = serde_json_core::to_slice(value, &mut payload).map_err(|_| ())?;

        // [magic][len][payload], padded up to a 4-byte (WORD_SIZE) write boundary.
        let mut buf = [0u8; 8 + MAX_PAYLOAD + 4];
        buf[0..4].copy_from_slice(&magic.to_le_bytes());
        buf[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        buf[8..8 + len].copy_from_slice(&payload[..len]);
        let total = (8 + len).div_ceil(4) * 4;

        let base = self.nvs_offset + sector;
        self.flash.erase(base, base + SECTOR).map_err(|_| ())?;
        self.flash.write(base, &buf[..total]).map_err(|_| ())?;

        // Immediately read the header back to prove the write physically landed. If this
        // shows the magic but a later reboot read does not, something is wiping the sector
        // between save and boot (rather than the write failing).
        let mut verify = [0u8; 8];
        let _ = self.flash.read(base, &mut verify);
        let rb_magic = u32::from_le_bytes(verify[0..4].try_into().unwrap());
        info!(
            "storage: WRITE @{base:#x} magic={magic:#x} len={len} total={total} → readback magic={rb_magic:#x}"
        );
        Ok(())
    }

    /// Read the whole persisted record (or an empty default if none/invalid).
    fn read_all(&mut self) -> Persisted {
        let mut scratch = [0u8; MAX_PAYLOAD];
        self.read_record(STORE_SECTOR, MAGIC, &mut scratch)
            .unwrap_or_default()
    }

    /// Write the whole persisted record back to the single store sector.
    fn write_all(&mut self, all: &Persisted) -> Result<(), ()> {
        self.write_record(STORE_SECTOR, MAGIC, all)
    }

    pub fn load_wifi(&mut self) -> Option<WifiCreds> {
        self.read_all().wifi
    }

    pub fn save_wifi(&mut self, creds: &WifiCreds) -> Result<(), ()> {
        let mut all = self.read_all();
        all.wifi = Some(creds.clone());
        self.write_all(&all)
    }

    /// Clear WiFi credentials only, leaving the saved selection intact (UC3, brief §7.9).
    pub fn clear_wifi(&mut self) -> Result<(), ()> {
        let mut all = self.read_all();
        all.wifi = None;
        self.write_all(&all)
    }

    pub fn load_selection(&mut self) -> Option<Selection> {
        self.read_all().selection
    }

    pub fn save_selection(&mut self, sel: &Selection) -> Result<(), ()> {
        let mut all = self.read_all();
        all.selection = Some(sel.clone());
        self.write_all(&all)
    }
}

/// Initialise the global [`STORE`]. Call once, early in boot.
pub async fn init(flash_periph: FLASH<'static>) -> Result<(), ()> {
    let store = Store::new(flash_periph)?;
    *STORE.lock().await = Some(store);
    Ok(())
}
