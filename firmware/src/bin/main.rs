#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

//! Zügli firmware entry point and orchestration (PROJECT_BRIEF.md §7.3, build order §9).
//!
//! Boot logic:
//! 1. Bring up the HAL, heap, RTOS scheduler, flash store, and the dual-core HUB75 display.
//! 2. If WiFi credentials exist → STA mode: serve the config page, sync time, poll & render
//!    (Phases 2 + 3). Otherwise → captive portal (Phase 1).
//! 3. A BOOT-button hold (3 s) wipes the WiFi creds + saved connection and reboots (UC3).

use embassy_executor::Spawner;
use embassy_net::{Config as NetConfig, StackResources};
use embassy_time::{Duration, Instant, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Pin, Pull};
use esp_hal::interrupt::Priority;
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::rng::Rng;
use esp_hal::system::Stack;
use esp_hal::timer::timg::TimerGroup;
use log::info;
use static_cell::ConstStaticCell;

use firmware::display::{self, Hub75Peripherals};
use firmware::httpd::config_server_task;
use firmware::mdns::mdns_task;
use firmware::model::DisplayState;
use firmware::poll::poll_task;
use firmware::shared::{self, DISPLAY, SELECTION, SELECTION_CHANGED};
use firmware::storage::{self, STORE};
use firmware::{mk_static, portal, sntp, wifi};

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let p = esp_hal::init(config);

    // Internal-RAM heap from reclaimed bootloader RAM (no `.bss` cost) — keeps WiFi's
    // DMA-capable allocations in on-chip SRAM. The large TLS/HTTP buffers live in PSRAM.
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73_744);
    // 8 MB external PSRAM (N16R8) added as a second heap region for the big poll buffers.
    esp_alloc::psram_allocator!(p.PSRAM, esp_hal::psram);

    let timg0 = TimerGroup::new(p.TIMG0);
    let sw_ints = SoftwareInterruptControl::new(p.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_ints.software_interrupt0);
    info!("zugli: boot");

    // --- Persistence ---
    if storage::init(p.FLASH).await.is_err() {
        log::error!("storage: could not find nvs partition");
    }

    // --- Display on the second core (brief §7.6) ---
    let (ex_a, ex_b) = display::exchanges();
    let (fb0, fb1) = display::framebuffers();
    let hub = Hub75Peripherals {
        lcd_cam: p.LCD_CAM,
        dma_channel: p.DMA_CH0,
        red1: p.GPIO38.degrade(),
        grn1: p.GPIO42.degrade(),
        blu1: p.GPIO48.degrade(),
        red2: p.GPIO47.degrade(),
        grn2: p.GPIO2.degrade(),
        blu2: p.GPIO21.degrade(),
        addr0: p.GPIO14.degrade(),
        addr1: p.GPIO46.degrade(),
        addr2: p.GPIO13.degrade(),
        addr3: p.GPIO9.degrade(),
        addr4: p.GPIO3.degrade(),
        blank: p.GPIO11.degrade(),
        clock: p.GPIO12.degrade(),
        latch: p.GPIO10.degrade(),
    };
    let sw2 = sw_ints.software_interrupt2;
    let cpu1 = move || {
        use esp_rtos::embassy::{Executor, InterruptExecutor};
        let hp = mk_static!(InterruptExecutor<2>, InterruptExecutor::new(sw2));
        let hp_spawner = hp.start(Priority::Priority3);
        hp_spawner
            .spawn(display::hub75_task(hub, ex_b, ex_a, fb1).unwrap());
        let lp = mk_static!(Executor, Executor::new());
        lp.run(|spawner| {
            spawner.spawn(display::render_task(ex_a, ex_b, fb0).unwrap());
        });
    };
    // Generous stack for the render core: the scrolling-marquee path nests embedded-graphics
    // text rendering for two animated headings plus the large time font every frame. Placed
    // in static memory via ConstStaticCell — `Stack::new()` is const, so the 32 KB value is
    // built at compile time and NOT materialised as a temporary on main's own stack (doing so
    // bloated main's frame and overflowed its stack at boot, in `framebuffers()`).
    static APP_CORE_STACK: ConstStaticCell<Stack<32768>> = ConstStaticCell::new(Stack::new());
    let app_core_stack = APP_CORE_STACK.take();
    esp_rtos::start_second_core(p.CPU_CTRL, sw_ints.software_interrupt1, app_core_stack, cpu1);

    // --- Random seed for the network stack + TLS ---
    let rng = Rng::new();
    let seed = ((rng.random() as u64) << 32) | rng.random() as u64;

    // --- Load persisted state ---
    let creds = {
        let mut g = STORE.lock().await;
        g.as_mut().and_then(|s| s.load_wifi())
    };
    let selection = {
        let mut g = STORE.lock().await;
        g.as_mut().and_then(|s| s.load_selection())
    };
    info!(
        "boot: loaded from flash — wifi creds {}, selection {}",
        if creds.is_some() { "present" } else { "absent" },
        if selection.is_some() { "present" } else { "absent" },
    );

    // --- WiFi controller + interfaces ---
    let (controller, interfaces) =
        esp_radio::wifi::new(p.WIFI, Default::default()).expect("wifi init");

    // --- BOOT-button WiFi reset (UC3) ---
    let button = Input::new(p.GPIO0, InputConfig::default().with_pull(Pull::Up));
    spawner.spawn(button_task(button).unwrap());

    match creds {
        None => {
            // ---------------- Phase 1: captive portal ----------------
            info!("zugli: no creds → captive portal");
            DISPLAY.signal(DisplayState::Provisioning);

            let device = interfaces.access_point;
            let resources = mk_static!(StackResources<8>, StackResources::new());
            let (stack, runner) = embassy_net::new(device, portal::ap_net_config(), resources, seed);

            spawner.spawn(wifi::net_task(runner).unwrap());
            spawner.spawn(portal::portal_wifi_task(controller).unwrap());
            spawner.spawn(portal::dhcp_task(stack).unwrap());
            spawner.spawn(portal::dns_task(stack).unwrap());
            spawner.spawn(portal::setup_server_task(stack).unwrap());
        }
        Some(creds) => {
            // ---------------- Phase 2 + 3: connected ----------------
            info!("zugli: joining {}", creds.ssid.as_str());
            let mut controller = controller;
            let _ = wifi::apply_sta(&mut controller, &creds);

            let device = interfaces.station;
            let resources = mk_static!(StackResources<8>, StackResources::new());
            let (stack, runner) =
                embassy_net::new(device, NetConfig::dhcpv4(Default::default()), resources, seed);

            spawner.spawn(wifi::net_task(runner).unwrap());
            spawner.spawn(wifi::sta_connection_task(controller).unwrap());

            if let Some(sel) = selection {
                *SELECTION.lock().await = Some(sel);
            }

            spawner.spawn(net_ready_task(stack).unwrap());
            spawner.spawn(config_server_task(stack).unwrap());
            spawner.spawn(mdns_task(stack).unwrap());
            spawner.spawn(poll_task(stack, seed).unwrap());
        }
    }

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}

/// Once the network is up: record the IP, show the idle/address screen if nothing is
/// selected yet (brief §7.7), then sync the clock via SNTP and keep it fresh (brief §7.4).
#[embassy_executor::task]
async fn net_ready_task(stack: embassy_net::Stack<'static>) {
    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        let octets = cfg.address.address().octets();
        shared::set_device_ip(octets);
        info!("net: ip = {}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3]);
        if SELECTION.lock().await.is_none() {
            DISPLAY.signal(DisplayState::IdleAddress { octets });
        }
    }

    // Initial SNTP sync, retrying until it lands.
    loop {
        if let Some(unix) = sntp::sync(stack).await {
            shared::set_clock(unix);
            SELECTION_CHANGED.signal(()); // re-poll now that minutes can be computed
            break;
        }
        Timer::after(Duration::from_secs(10)).await;
    }

    // Periodic resync.
    loop {
        Timer::after(Duration::from_secs(3600)).await;
        if let Some(unix) = sntp::sync(stack).await {
            shared::set_clock(unix);
        }
    }
}

/// Poll the BOOT button (GPIO0). A continuous 3 s hold wipes the stored WiFi credentials
/// and the saved connection, then reboots into the captive portal (UC3, brief §7.9).
#[embassy_executor::task]
async fn button_task(button: Input<'static>) {
    const HOLD: Duration = Duration::from_secs(3);
    loop {
        if button.is_low() {
            let start = Instant::now();
            while button.is_low() {
                if start.elapsed() >= HOLD {
                    info!("button: 3s hold → clearing WiFi creds + connection, rebooting");
                    {
                        let mut g = STORE.lock().await;
                        if let Some(store) = g.as_mut() {
                            let _ = store.clear_all();
                        }
                    }
                    Timer::after(Duration::from_millis(200)).await;
                    esp_hal::system::software_reset();
                }
                Timer::after(Duration::from_millis(100)).await;
            }
        }
        Timer::after(Duration::from_millis(100)).await;
    }
}
