//! Phase 2 configuration HTTP server (PROJECT_BRIEF.md §4).
//!
//! Serves the self-contained config page and accepts `POST /save`. This server stays up the
//! whole time the device runs (brief §2 "config stays live") so the user can re-pick a stop
//! at any time. On save we persist the selection and signal the poll task to switch live —
//! no reboot (brief §4.4).

use core::fmt::Write as _;
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_net::Stack;
use embassy_time::Duration;
use heapless::String;
use log::info;
use picoserve::extract::Json;
use picoserve::response::{Content, IntoResponse, Response};
use picoserve::routing::{get, post};
use picoserve::io::Write;
use picoserve::{Config, Router, Timeouts};
use static_cell::StaticCell;

use crate::model::{Config as BoardConfig, Layout, Selection};
use crate::shared::{self, SELECTION, SELECTION_CHANGED};
use crate::storage::{self, STORE};

/// The config page, embedded into the firmware so it is served even without internet access for
/// the device itself (the page's own API calls go out over the phone's link). Stored **gzip-
/// compressed** (built by build.rs) and served with `Content-Encoding: gzip` — the plaintext page
/// is ~118 KB, and serving it whole once drained the WiFi driver's static TX buffer pool on a
/// reload (the `esp_wifi_internal_tx returned error: 257` / NO_MEM backpressure). Compressing it
/// ~5-7× keeps the burst under that pressure point; the bytes sit in flash `.rodata`, costing no RAM.
const INDEX_HTML_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/index.html.gz"));

/// A **borrowed** response body with an explicit content type. This is the workhorse response
/// type for a memory-budget reason (VERIFY.md M0): picoserve's response state machine keeps ~20
/// by-value copies of the `Content` in the handler's future, so a `Body` (one fat pointer, 16 B)
/// costs ~320 B where an owned `String<1536>` or a serde-`Json<Layout>` would inflate the task's
/// future by ~20–30 KB of `.bss` — starving the shared core-0 stack the poll TLS path runs on.
/// Big JSON is therefore serialized synchronously into a static buffer ([`raw_json`]) and only
/// the borrowed bytes cross the `.await`.
pub struct Body {
    pub content_type: &'static str,
    pub body: &'static [u8],
}

impl Content for Body {
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

pub fn html(body: &'static str) -> Body {
    Body {
        content_type: "text/html; charset=utf-8",
        body: body.as_bytes(),
    }
}

pub fn json(body: &'static str) -> Body {
    Body {
        content_type: "application/json",
        body: body.as_bytes(),
    }
}

/// The `{"ok":true}` acknowledgement every mutating endpoint returns.
fn ok() -> Body {
    json("{\"ok\":true}")
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
    Response::ok(Body {
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
    // Persist to flash, update the live selection, and wake the poll task. A failed persist
    // still works this session (set in memory below) but is lost on reboot — exactly the
    // "polling doesn't start after restart" symptom the persist log calls out.
    storage::persist("selection", |s| s.save_selection(&sel)).await;
    *SELECTION.lock().await = Some(sel);
    SELECTION_CHANGED.signal(());
    Response::ok(ok())
}

/// Capacity for the serialized config JSON. Sized for exactly the field set written below
/// (`stripCity`, `showLineBadges`, `brightness`, `autoBrightness`, `offWhenDimmed`, `reducedStart`,
/// `reducedEnd`, `uiMode` — ~150 B today) with headroom; if a future field pushes past it we return
/// `{}` rather than a truncated, unparseable object (see [`get_config`]).
const CONFIG_JSON_CAP: usize = 224;

/// Current board settings, for the config page's settings sheet to pre-fill its controls.
async fn get_config() -> impl IntoResponse {
    let mut body: String<CONFIG_JSON_CAP> = String::new();
    if write!(
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
    )
    .is_err()
    {
        // Overflow means a newly added field outgrew CONFIG_JSON_CAP: emit valid-but-empty JSON
        // instead of a half-written object. The page reads `{}` as "no settings" and keeps its
        // current control values, and the error names the fix (bump the cap).
        log::error!("config: serialized config exceeds {CONFIG_JSON_CAP} B — returning {{}} (raise CONFIG_JSON_CAP)");
        body.clear();
        let _ = body.push_str("{}");
    }
    Response::ok(OwnedJson(body))
}

/// Apply a settings change: persist it and update the live mirror the panel reads. Affects the
/// board immediately — a re-poll re-emits the departures screen, redrawn with the new settings.
async fn set_config(Json(cfg): Json<BoardConfig>) -> impl IntoResponse {
    info!(
        "config: stripCity={} showLineBadges={} brightness={} autoBrightness={} offWhenDimmed={} reduced={}..{} uiMode={}",
        cfg.strip_city, cfg.show_line_badges, cfg.brightness, cfg.auto_brightness, cfg.off_when_dimmed, cfg.reduced_start, cfg.reduced_end, cfg.ui_mode
    );
    storage::persist("config", |s| s.save_config(&cfg)).await;
    shared::apply_config(&cfg);
    SELECTION_CHANGED.signal(()); // wake the poll task so the panel redraws now
    Response::ok(ok())
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

/// Reentrancy guard for [`JSON_TX_BUF`] (T2.3): set on entry to `raw_json`, cleared once the write +
/// slice build are done. A failed `compare_exchange` means two `raw_json` serializations overlapped
/// — impossible today (`serve()` handles one connection at a time; see its SAFETY-INVARIANT), so it
/// would signal a future concurrency regression that silently aliased the shared buffer. This
/// hardens the existing single-connection invariant into something enforceable rather than
/// replacing it: it guards the *write window*, and the returned slice stays sound because `serve()`
/// fully flushes each response before the next request (so the buffer isn't overwritten under it).
static JSON_TX_IN_USE: AtomicBool = AtomicBool::new(false);

/// Serialize `value` into [`JSON_TX_BUF`] with **serde-json-core** (a synchronous call) and hand
/// out the borrowed slice as a [`Body`]. Serializing synchronously matters as much as borrowing:
/// picoserve's own `response::Json` drives serde into an *async* writer, so a deep type's whole
/// recursive serialize state would live in the handler's future (~22 KB of `.bss` for a `Layout`).
/// An over-budget serialize (can't happen for values that respect the schema bounds) degrades to
/// `{}` rather than failing the request.
fn raw_json<T: serde::Serialize>(value: &T) -> Body {
    // Claim the shared buffer for the duration of the serialize + slice build. A failed claim means
    // a concurrent `raw_json` (see [`JSON_TX_IN_USE`]): panic in debug to catch it in test, and in
    // release degrade to `{}` rather than hand out a slice of a buffer another writer is mid-write.
    if JSON_TX_IN_USE
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        debug_assert!(false, "raw_json re-entered: JSON_TX_BUF aliased (serve() must be single-connection)");
        log::error!("raw_json re-entered concurrently — returning empty JSON");
        return Body { content_type: "application/json", body: b"{}" };
    }
    let ptr = core::ptr::addr_of_mut!(JSON_TX_BUF);
    let len = serde_json_core::to_slice(value, unsafe { &mut *ptr }).unwrap_or(0);
    let body: &'static [u8] = if len == 0 {
        b"{}"
    } else {
        unsafe { core::slice::from_raw_parts(ptr as *const u8, len) }
    };
    // Write done → release the guard. The returned slice remains valid because `serve()` fully
    // writes each response before accepting the next request (its SAFETY-INVARIANT), so no other
    // `raw_json` overwrites these bytes before they are flushed.
    JSON_TX_IN_USE.store(false, Ordering::Release);
    Body { content_type: "application/json", body }
}

/// The persisted custom layout as JSON, for the editor to seed its working copy and for the
/// main-page thumbnail. Returns `{"v":1,"e":[]}` when no layout is saved.
async fn get_layout() -> impl IntoResponse {
    let layout = {
        let mut guard = STORE.lock().await;
        guard.as_mut().and_then(|s| s.load_layout())
    }
    .unwrap_or_default(); // Layout::default() serializes to {"v":1,"e":[]}
    Response::ok(raw_json(&layout))
}

/// The current panel selection as JSON, so the config page can restore its main-page state (chosen
/// stop, tracking mode, picked connections) the first time it opens — the settings sheet and custom
/// layout already pre-fill from `/config` and `/layout`. Mirrors the `POST /save` body shape.
/// Returns `{}` when nothing is selected yet, which the page reads as "leave the form blank".
async fn get_selection() -> impl IntoResponse {
    // Clone out of the live mirror (seeded from flash at boot, updated on every save) so the lock is
    // released before serializing, and the clone is dropped before the response's write `.await`.
    let selection = { SELECTION.lock().await.clone() };
    Response::ok(match selection {
        Some(sel) => raw_json(&sel),
        None => json("{}"),
    })
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
    info!("layout: saving ({} elements)", layout.e.len());
    storage::persist("layout", |s| s.save_layout(if empty { None } else { Some(&layout) })).await;
    warn_low_heap("layout");
    shared::apply_layout(if empty { None } else { Some(layout) });
    shared::end_preview(); // a save supersedes any in-flight preview (§7.4); no-op if none active
    SELECTION_CHANGED.signal(()); // wake the render/poll path so the panel redraws now
    Response::ok(ok())
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
    Response::ok(ok())
}

/// End the live preview (editor Cancel; harmless after Save): drop the transient layout and revert
/// the panel to the device's persisted UI mode + layout (§7.4).
async fn end_preview() -> impl IntoResponse {
    shared::end_preview();
    Response::ok(ok())
}

/// The three network buffers [`serve`] needs, bundled so a **single** [`StaticCell`]
/// ([`SERVER_BUFFERS`]) backs both the config server and the captive-portal setup server. Only one
/// of those two tasks ever runs per boot (STA config XOR provisioning), but both task futures link
/// into the binary; without sharing, each would reserve its own 7,168 B set in `.bss`. Sizes are the
/// proven values (see [`serve`]): tcp_rx windows a body across segments, tcp_tx holds the largest
/// response, http_buf a `/layout` body + headers.
pub struct ServerBuffers {
    tcp_rx: [u8; 1024],
    tcp_tx: [u8; 4096],
    http_buf: [u8; 2048],
}

/// Backing store for the one shared [`ServerBuffers`] set, taken at runtime by whichever server task
/// actually spawns.
static SERVER_BUFFERS: StaticCell<ServerBuffers> = StaticCell::new();

/// Take the shared server buffers. **Panics if called more than once per boot** — which is exactly
/// what enforces the "only one HTTP server per boot" invariant (STA config server XOR captive-portal
/// setup server) that sharing one buffer set makes load-bearing. Call once, from the single server
/// task that runs.
///
/// Zeroed **in place** rather than built via `init(ServerBuffers { .. })`, which would materialise
/// the 7,168 B struct as a stack temporary before the memcpy into the cell — the same hazard
/// `display::framebuffers()` documents (its 28 KB temporary overflowed the boot stack).
pub fn server_buffers() -> &'static mut ServerBuffers {
    let slot = SERVER_BUFFERS.uninit();
    // SAFETY: `ServerBuffers` is three plain `[u8; N]` arrays — all-zero bytes are a valid,
    // fully-initialised value.
    unsafe {
        slot.as_mut_ptr().write_bytes(0, 1);
        slot.assume_init_mut()
    }
}

/// Run a picoserve HTTP server on port 80, forever — the loop shared by this config server and
/// the captive-portal setup server. Handles one connection at a time, re-listening after each
/// connection closes.
///
/// The buffers live in one shared static ([`SERVER_BUFFERS`], taken via [`server_buffers`]) rather
/// than as locals in the calling task's future. They still sit in static DRAM either way, but only
/// one server runs per boot (STA config XOR provisioning), so sharing reserves ONE set instead of
/// baking a copy into each of the two task futures — halving the reservation that pushes down the
/// core-0 main-task stack floor (`_stack_end_cpu0`), where the poll task's TLS handshake +
/// stationboard JSON parse run and are already near the limit (see poll.rs / commit 78c5126). Keep
/// them at the proven sizes: http_buf 2048 holds a realistic `/layout` body (~0.5 KB, worst-case
/// valid ~1.5 KB) plus headers, and tcp_rx 1024 windows a larger body across segments.
pub async fn serve<P: picoserve::routing::PathRouter>(
    app: &Router<P>,
    name: &'static str,
    stack: Stack<'static>,
    buffers: &'static mut ServerBuffers,
) -> ! {
    let config = Config::new(Timeouts {
        start_read_request: Duration::from_secs(10),
        persistent_start_read_request: Duration::from_secs(5),
        read_request: Duration::from_secs(10),
        write: Duration::from_secs(10),
    });

    loop {
        // SAFETY-INVARIANT: this loop serves ONE connection at a time and fully writes each response
        // — including the borrowed `raw_json`/`JSON_TX_BUF` slices — before `listen_and_serve`
        // returns and the next request is accepted. That serialization is what makes the shared
        // `static mut JSON_TX_BUF` slice sound and the `JSON_TX_IN_USE` guard's clear-on-exit
        // correct; a move to concurrent connections would break both.
        let _ = picoserve::Server::new(app, &config, &mut buffers.http_buf)
            .listen_and_serve(name, stack, 80, &mut buffers.tcp_rx, &mut buffers.tcp_tx)
            .await;
    }
}

/// Serve the config page + `/save` on port 80. Stays up the whole time the device runs
/// (brief §2 "config stays live").
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
    serve(&app, "config", stack, server_buffers()).await
}
