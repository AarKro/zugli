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

use crate::model::{Selection, WifiCreds};

const SECTOR: u32 = FlashStorage::SECTOR_SIZE; // 4096
const WIFI_SECTOR: u32 = 0;
const SELECTION_SECTOR: u32 = SECTOR;

const MAGIC_WIFI: u32 = 0x5A47_4C57; // "ZGLW"
const MAGIC_SEL: u32 = 0x5A47_4C53; // "ZGLS"
const MAX_PAYLOAD: usize = 512;

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
        self.flash.read(base, &mut header).ok()?;
        let found_magic = u32::from_le_bytes(header[0..4].try_into().unwrap());
        if found_magic != magic {
            return None;
        }
        let len = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        if len == 0 || len > MAX_PAYLOAD {
            return None;
        }
        self.flash.read(base + 8, &mut scratch[..len]).ok()?;
        serde_json_core::from_slice::<T>(&scratch[..len])
            .ok()
            .map(|(v, _)| v)
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
        self.flash.write(base, &buf[..total]).map_err(|_| ())
    }

    fn erase_sector(&mut self, sector: u32) -> Result<(), ()> {
        let base = self.nvs_offset + sector;
        self.flash.erase(base, base + SECTOR).map_err(|_| ())
    }

    pub fn load_wifi(&mut self) -> Option<WifiCreds> {
        let mut scratch = [0u8; MAX_PAYLOAD];
        self.read_record(WIFI_SECTOR, MAGIC_WIFI, &mut scratch)
    }

    pub fn save_wifi(&mut self, creds: &WifiCreds) -> Result<(), ()> {
        self.write_record(WIFI_SECTOR, MAGIC_WIFI, creds)
    }

    /// Clear WiFi credentials only, leaving the saved selection intact (UC3, brief §7.9).
    pub fn clear_wifi(&mut self) -> Result<(), ()> {
        self.erase_sector(WIFI_SECTOR)
    }

    pub fn load_selection(&mut self) -> Option<Selection> {
        let mut scratch = [0u8; MAX_PAYLOAD];
        self.read_record(SELECTION_SECTOR, MAGIC_SEL, &mut scratch)
    }

    pub fn save_selection(&mut self, sel: &Selection) -> Result<(), ()> {
        self.write_record(SELECTION_SECTOR, MAGIC_SEL, sel)
    }
}

/// Initialise the global [`STORE`]. Call once, early in boot.
pub async fn init(flash_periph: FLASH<'static>) -> Result<(), ()> {
    let store = Store::new(flash_periph)?;
    *STORE.lock().await = Some(store);
    Ok(())
}
