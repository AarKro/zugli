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

use crate::model::{Config as BoardConfig, Layout, Selection};
use crate::shared::{self, SELECTION, SELECTION_CHANGED};
use crate::storage::STORE;

/// The config page, embedded into the firmware so it is served even without internet access for
/// the device itself (the page's own API calls go out over the phone's link). Stored **gzip-
/// compressed** (built by build.rs) and served with `Content-Encoding: gzip` — the plaintext page
/// is ~118 KB, and serving it whole once drained the WiFi driver's static TX buffer pool on a
/// reload (the `esp_wifi_internal_tx returned error: 257` / NO_MEM backpressure). Compressing it
/// ~5-7× keeps the burst under that pressure point; the bytes sit in flash `.rodata`, costing no RAM.
const INDEX_HTML_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/index.html.gz"));

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

/// A borrowed-bytes body with an explicit content type — like [`Static`] but for non-UTF-8
/// payloads (the gzip-compressed config page). Holds only a fat pointer, so picoserve's
/// response state machine stays cheap (see [`RawJson`]).
pub struct StaticBytes {
    pub content_type: &'static str,
    pub body: &'static [u8],
}

impl Content for StaticBytes {
    fn content_type(&self) -> &'static str {
        self.content_type
    }
    fn content_length(&self) -> usize {
        self.body.len()
    }
    async fn write_content<W: Write>(self, mut writer: W) -> Result<(), W::Error> {
        writer.write_all(self.body).await
    }
}

pub fn json(body: &'static str) -> Static {
    Static {
        content_type: "application/json",
        body,
    }
}

/// A JSON response for a `Layout`, serialized with **serde-json-core** (a synchronous call) rather
/// than picoserve's `response::Json`. picoserve's serializer drives a serde `Serialize` into an
/// *async* writer, so serializing a deep type (`Layout` = `Vec<Element, 16>` of 15-field structs)
/// keeps the whole recursive serialize state in the handler's future — ~22 KB of `.bss` that steals
/// from the core-0 stack the poll TLS path needs. Here the serialize happens synchronously into a
/// stack buffer and only the bytes are written across the `.await`, so the future stays small.
/// A JSON response whose body is a **borrowed** byte slice. Holds only a fat pointer (16 B), so
/// picoserve's response state machine — which keeps ~20 by-value copies of the `Content` in its
/// future — costs ~320 B instead of ~20× the body size. (An owned `String<1536>` or a `Layout`
/// held here inflates this task's future by ~20–30 KB of `.bss`, starving the shared core-0 stack
/// the poll TLS path runs on.) The bytes must outlive the response; see [`get_layout`].
struct RawJson(&'static [u8]);

impl Content for RawJson {
    fn content_type(&self) -> &'static str {
        "application/json"
    }
    fn content_length(&self) -> usize {
        self.0.len()
    }
    async fn write_content<W: Write>(self, mut writer: W) -> Result<(), W::Error> {
        writer.write_all(self.0).await
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
    // Served pre-gzipped (see `INDEX_HTML_GZ`); the browser transparently inflates it. The
    // `charset=utf-8` describes the *decompressed* body, per HTTP — `Content-Encoding` is the
    // transfer wrapper, `Content-Type` the underlying media type.
    Response::ok(StaticBytes {
        content_type: "text/html; charset=utf-8",
        body: INDEX_HTML_GZ,
    })
    .with_header("Content-Encoding", "gzip")
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
    let mut body: String<224> = String::new();
    let _ = write!(
        body,
        "{{\"stripCity\":{},\"showLineBadges\":{},\"brightness\":{},\"autoBrightness\":{},\"offWhenDimmed\":{},\"reducedStart\":{},\"reducedEnd\":{},\"uiMode\":{}}}",
        shared::strip_city_enabled(),
        shared::line_badges_enabled(),
        shared::brightness_level(),
        shared::auto_brightness_enabled(),
        shared::off_when_dimmed_enabled(),
        shared::reduced_start_min(),
        shared::reduced_end_min(),
        shared::ui_mode() as u8,
    );
    Response::ok(OwnedJson(body))
}

/// Apply a settings change: persist it and update the live mirror the panel reads. Affects the
/// board immediately — a re-poll re-emits the departures screen, redrawn with the new settings.
async fn set_config(Json(cfg): Json<BoardConfig>) -> impl IntoResponse {
    info!(
        "config: stripCity={} showLineBadges={} brightness={} autoBrightness={} offWhenDimmed={} reduced={}..{} uiMode={}",
        cfg.strip_city, cfg.show_line_badges, cfg.brightness, cfg.auto_brightness, cfg.off_when_dimmed, cfg.reduced_start, cfg.reduced_end, cfg.ui_mode
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

/// Shared scratch for the borrowed-slice JSON responses ([`get_layout`], [`get_selection`]). Sized
/// for the larger user (`Layout`, `LAYOUT_MAX_BYTES` ≈ 1.5 KB); a worst-case `Selection` serializes
/// to well under 1 KB, so it fits with headroom. Reusing one buffer keeps `.bss` flat instead of
/// adding a second ~1 KB static — the RAM budget is knife-edge (the poll TLS path shares the core-0
/// stack). SAFETY: `config_server_task` serves a single connection at a time, so each handler
/// serializes into this buffer and fully writes the borrowed slice out before the next request is
/// accepted — never concurrent access, and the `&mut` for the write is dropped before the borrow.
static mut JSON_TX_BUF: [u8; crate::model::LAYOUT_MAX_BYTES] =
    [0u8; crate::model::LAYOUT_MAX_BYTES];

/// The persisted custom layout as JSON, for the editor to seed its working copy and for the
/// main-page thumbnail. Returns `{"v":1,"e":[]}` when no layout is saved.
async fn get_layout() -> impl IntoResponse {
    let layout = {
        let mut guard = STORE.lock().await;
        guard.as_mut().and_then(|s| s.load_layout())
    }
    .unwrap_or_default(); // Layout::default() serializes to {"v":1,"e":[]}

    // Serialize into the shared static buffer and serve a borrowed slice (see `RawJson`/`JSON_TX_BUF`
    // for why owned responses blow the memory budget).
    let ptr = core::ptr::addr_of_mut!(JSON_TX_BUF);
    let len = serde_json_core::to_slice(&layout, unsafe { &mut *ptr }).unwrap_or(0);
    Response::ok(RawJson(unsafe { core::slice::from_raw_parts(ptr as *const u8, len) }))
}

/// The current panel selection as JSON, so the config page can restore its main-page state (chosen
/// stop, tracking mode, picked connections) the first time it opens — the settings sheet and custom
/// layout already pre-fill from `/config` and `/layout`. Mirrors the `POST /save` body shape.
/// Returns `{}` when nothing is selected yet, which the page reads as "leave the form blank".
async fn get_selection() -> impl IntoResponse {
    // Clone out of the live mirror (seeded from flash at boot, updated on every save) so the lock is
    // released before serializing, and the clone is dropped before the response's write `.await`.
    let selection = { SELECTION.lock().await.clone() };
    let ptr = core::ptr::addr_of_mut!(JSON_TX_BUF);
    let bytes: &'static [u8] = match selection {
        Some(sel) => {
            let len = serde_json_core::to_slice(&sel, unsafe { &mut *ptr }).unwrap_or(0);
            if len == 0 {
                b"{}" // over-budget serialize (won't happen for a valid Selection) → degrade to blank
            } else {
                unsafe { core::slice::from_raw_parts(ptr as *const u8, len) }
            }
        }
        None => b"{}",
    };
    Response::ok(RawJson(bytes))
}

/// Persist a custom layout (FEATURE_UI_BUILDER §7.4). Clamps every field to its valid range; an
/// empty `e` clears the saved layout. On success the live mirror is updated and a redraw is
/// signalled (the same wake `/save` and `/config` use).
///
/// No explicit byte-budget check is needed: parsing already bounds the layout to `MAX_ELEMENTS`
/// elements and `String<24>` text, whose worst-case serialization (~1465 B) is < `LAYOUT_MAX_BYTES`
/// and well under `MAX_PAYLOAD`, so `save_layout`'s own serialize never overflows for a *parsed*
/// layout — and it returns `Err` (logged, no partial write) if it somehow did. Crucially, no large
/// scratch buffer is held here: buffers that live across the `.await` below would be baked into this
/// task's future (i.e. `.bss`), shrinking the shared core-0 stack the poll TLS path runs on.
async fn set_layout(Json(mut layout): Json<Layout>) -> impl IntoResponse {
    layout.sanitize();

    // An empty layout means "no custom layout" — stored as `None` so it and a never-saved layout
    // are indistinguishable on disk (and in Custom mode both fall back to the built-in board).
    let empty = layout.e.is_empty();
    {
        let mut guard = STORE.lock().await;
        let stored = if empty { None } else { Some(&layout) };
        match guard.as_mut() {
            Some(store) => match store.save_layout(stored) {
                Ok(()) => info!("layout: persisted ({} elements)", layout.e.len()),
                Err(()) => log::error!("layout: FLASH SAVE FAILED (over-budget or no flash)"),
            },
            None => log::error!("layout: no flash store (won't survive reboot)"),
        }
    }
    warn_low_heap("layout");
    shared::apply_layout(if empty { None } else { Some(layout) });
    shared::end_preview(); // a save supersedes any in-flight preview (§7.4); no-op if none active
    SELECTION_CHANGED.signal(()); // wake the render/poll path so the panel redraws now
    Response::ok(json("{\"ok\":true}"))
}

/// Push a **transient** live preview of the working layout (FEATURE_UI_BUILDER §7.4 / §4.3).
/// Validates/clamps like [`set_layout`] but writes **no flash** — this is the editor's high-frequency
/// endpoint (debounced edits + a ~5 s keepalive), so it must never touch the sector. It (re)arms the
/// ~15 s auto-revert deadline and forces an immediate, poll-free redraw so the panel tracks the
/// design. The preview overrides the persisted UI mode until it ends or lapses.
/// Internal (DMA-capable) free-heap floor below which WiFi TX starts failing with `ESP_ERR_NO_MEM`
/// (seen as `esp_wifi_internal_tx returned error: 257`). Editor use once drove the board into an
/// intermittent silent freeze consistent with internal-RAM exhaustion; this is the cheap safety net
/// left behind — quiet in normal use, but if internal RAM ever creeps toward empty it surfaces in
/// the log instead of a silent hang. Tune if it proves too chatty / too quiet.
const LOW_HEAP_WARN_BYTES: usize = 6 * 1024;

/// Warn if the internal heap has fallen into the danger zone. Called from the editor's request path
/// (the load pattern that exposed the pressure). Reading free heap is cheap; it logs only when low.
fn warn_low_heap(tag: &str) {
    let internal = esp_alloc::HEAP.free_caps(esp_alloc::MemoryCapability::Internal.into());
    if internal < LOW_HEAP_WARN_BYTES {
        log::warn!("heap[{tag}]: internal free={internal} B is low — WiFi TX may fail (NO_MEM)");
    }
}

async fn set_preview(Json(mut layout): Json<Layout>) -> impl IntoResponse {
    layout.sanitize();
    warn_low_heap("preview");
    shared::set_preview(layout);
    Response::ok(json("{\"ok\":true}"))
}

/// End the live preview (editor Cancel; harmless after Save): drop the transient layout and revert
/// the panel to the device's persisted UI mode + layout (§7.4).
async fn end_preview() -> impl IntoResponse {
    shared::end_preview();
    Response::ok(json("{\"ok\":true}"))
}

/// Serve the config page + `/save` on port 80. Handles one connection at a time, so the
/// task simply re-listens after each connection closes.
#[embassy_executor::task]
pub async fn config_server_task(stack: Stack<'static>) {
    let app = Router::new()
        .route("/", get(index))
        .route("/save", post(save))
        .route("/selection", get(get_selection))
        .route("/config", get(get_config).post(set_config))
        .route("/layout", get(get_layout).post(set_layout))
        .route("/preview", post(set_preview))
        .route("/preview/end", post(end_preview));
    let config = Config::new(Timeouts {
        start_read_request: Duration::from_secs(10),
        persistent_start_read_request: Duration::from_secs(5),
        read_request: Duration::from_secs(10),
        write: Duration::from_secs(10),
    });

    // These live in the task's static arena, which sits in `.bss` and so pushes down the core-0
    // main-task stack floor (`_stack_end_cpu0`) — where the poll task's TLS handshake + stationboard
    // JSON parse run and are already near the limit (see poll.rs / commit 78c5126). Keep them at the
    // proven sizes: http_buf 2048 holds a realistic `/layout` body (~0.5 KB, worst-case valid
    // ~1.5 KB) plus headers, and tcp_rx 1024 windows a larger body across segments.
    let mut tcp_rx = [0u8; 1024];
    let mut tcp_tx = [0u8; 4096];
    let mut http_buf = [0u8; 2048];

    loop {
        let _ = picoserve::Server::new(&app, &config, &mut http_buf)
            .listen_and_serve("config", stack, 80, &mut tcp_rx, &mut tcp_tx)
            .await;
    }
}
