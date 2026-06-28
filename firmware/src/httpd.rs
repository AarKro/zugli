//! Phase 2 configuration HTTP server (PROJECT_BRIEF.md §4).
//!
//! Serves the self-contained config page and accepts `POST /save`. This server stays up the
//! whole time the device runs (brief §2 "config stays live") so the user can re-pick a stop
//! at any time. On save we persist the selection and signal the poll task to switch live —
//! no reboot (brief §4.4).

use core::fmt::Write as _;

use embassy_net::Stack;
use embassy_time::Duration;
use heapless::String;
use log::info;
use picoserve::extract::Json;
use picoserve::response::{Content, IntoResponse, Response};
use picoserve::routing::{get, post};
use picoserve::io::Write;
use picoserve::{Config, Router, Timeouts};

use crate::model::{Config as BoardConfig, Selection};
use crate::shared::{self, SELECTION, SELECTION_CHANGED};
use crate::storage::STORE;

/// The config page, embedded into the firmware so it is served even without internet
/// access for the device itself (the page's own API calls go out over the phone's link).
const INDEX_HTML: &str = include_str!("../../web/index.html");

/// A response body with an explicit content type.
pub struct Static {
    pub content_type: &'static str,
    pub body: &'static str,
}

impl Content for Static {
    fn content_type(&self) -> &'static str {
        self.content_type
    }
    fn content_length(&self) -> usize {
        self.body.len()
    }
    async fn write_content<W: Write>(self, mut writer: W) -> Result<(), W::Error> {
        writer.write_all(self.body.as_bytes()).await
    }
}

pub fn html(body: &'static str) -> Static {
    Static {
        content_type: "text/html; charset=utf-8",
        body,
    }
}

pub fn json(body: &'static str) -> Static {
    Static {
        content_type: "application/json",
        body,
    }
}

/// A small owned-body JSON response, for endpoints whose body is built at runtime.
pub struct OwnedJson<const N: usize>(pub String<N>);

impl<const N: usize> Content for OwnedJson<N> {
    fn content_type(&self) -> &'static str {
        "application/json"
    }
    fn content_length(&self) -> usize {
        self.0.len()
    }
    async fn write_content<W: Write>(self, mut writer: W) -> Result<(), W::Error> {
        writer.write_all(self.0.as_bytes()).await
    }
}

async fn index() -> impl IntoResponse {
    Response::ok(html(INDEX_HTML))
}

async fn save(Json(sel): Json<Selection>) -> impl IntoResponse {
    info!(
        "save: {} — mode {}, {} connection(s)",
        sel.stop_name.as_str(),
        if sel.all_connections { "all" } else { "specific" },
        sel.connections.len(),
    );
    // Persist to flash, update the live selection, and wake the poll task. Log the persist
    // result: if this fails the selection still works this session (set in memory below) but
    // is lost on reboot — exactly the "polling doesn't start after restart" symptom.
    {
        let mut guard = STORE.lock().await;
        match guard.as_mut() {
            Some(store) => match store.save_selection(&sel) {
                Ok(()) => info!("save: selection persisted to flash"),
                Err(()) => log::error!("save: FLASH SAVE FAILED (selection won't survive reboot)"),
            },
            None => log::error!("save: no flash store (selection won't survive reboot)"),
        }
    }
    *SELECTION.lock().await = Some(sel);
    SELECTION_CHANGED.signal(());
    Response::ok(json("{\"ok\":true}"))
}

/// Current board settings, for the config page's settings sheet to pre-fill its controls.
async fn get_config() -> impl IntoResponse {
    let mut body: String<160> = String::new();
    let _ = write!(
        body,
        "{{\"stripCity\":{},\"showLineBadges\":{},\"brightness\":{},\"autoBrightness\":{},\"reducedStart\":{},\"reducedEnd\":{}}}",
        shared::strip_city_enabled(),
        shared::line_badges_enabled(),
        shared::brightness_level(),
        shared::auto_brightness_enabled(),
        shared::reduced_start_min(),
        shared::reduced_end_min(),
    );
    Response::ok(OwnedJson(body))
}

/// Apply a settings change: persist it and update the live mirror the panel reads. Affects the
/// board immediately — a re-poll re-emits the departures screen, redrawn with the new settings.
async fn set_config(Json(cfg): Json<BoardConfig>) -> impl IntoResponse {
    info!(
        "config: stripCity={} showLineBadges={} brightness={} autoBrightness={} reduced={}..{}",
        cfg.strip_city, cfg.show_line_badges, cfg.brightness, cfg.auto_brightness, cfg.reduced_start, cfg.reduced_end
    );
    {
        let mut guard = STORE.lock().await;
        match guard.as_mut() {
            Some(store) => match store.save_config(&cfg) {
                Ok(()) => info!("config: persisted to flash"),
                Err(()) => log::error!("config: FLASH SAVE FAILED (setting won't survive reboot)"),
            },
            None => log::error!("config: no flash store (setting won't survive reboot)"),
        }
    }
    shared::apply_config(&cfg);
    SELECTION_CHANGED.signal(()); // wake the poll task so the panel redraws now
    Response::ok(json("{\"ok\":true}"))
}

/// Serve the config page + `/save` on port 80. Handles one connection at a time, so the
/// task simply re-listens after each connection closes.
#[embassy_executor::task]
pub async fn config_server_task(stack: Stack<'static>) {
    let app = Router::new()
        .route("/", get(index))
        .route("/save", post(save))
        .route("/config", get(get_config).post(set_config));
    let config = Config::new(Timeouts {
        start_read_request: Duration::from_secs(10),
        persistent_start_read_request: Duration::from_secs(5),
        read_request: Duration::from_secs(10),
        write: Duration::from_secs(10),
    });

    let mut tcp_rx = [0u8; 1024];
    let mut tcp_tx = [0u8; 4096];
    let mut http_buf = [0u8; 2048];

    loop {
        let _ = picoserve::Server::new(&app, &config, &mut http_buf)
            .listen_and_serve("config", stack, 80, &mut tcp_rx, &mut tcp_tx)
            .await;
    }
}
