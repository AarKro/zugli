//! Phase 2 configuration HTTP server (PROJECT_BRIEF.md §4).
//!
//! Serves the self-contained config page and accepts `POST /save`. This server stays up the
//! whole time the device runs (brief §2 "config stays live") so the user can re-pick a stop
//! at any time. On save we persist the selection and signal the poll task to switch live —
//! no reboot (brief §4.4).

use embassy_net::Stack;
use embassy_time::Duration;
use log::info;
use picoserve::extract::Json;
use picoserve::response::{Content, IntoResponse, Response};
use picoserve::routing::{get, post};
use picoserve::io::Write;
use picoserve::{Config, Router, Timeouts};

use crate::model::Selection;
use crate::shared::{SELECTION, SELECTION_CHANGED};
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

async fn index() -> impl IntoResponse {
    Response::ok(html(INDEX_HTML))
}

async fn save(Json(sel): Json<Selection>) -> impl IntoResponse {
    info!(
        "save: {} / {} → {}",
        sel.stop_name.as_str(),
        sel.line.as_str(),
        sel.destination.as_str()
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

/// Serve the config page + `/save` on port 80. Handles one connection at a time, so the
/// task simply re-listens after each connection closes.
#[embassy_executor::task]
pub async fn config_server_task(stack: Stack<'static>) {
    let app = Router::new()
        .route("/", get(index))
        .route("/save", post(save));
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
