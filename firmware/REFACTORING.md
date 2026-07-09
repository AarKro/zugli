# Firmware Refactoring Plan — Memory & Resource Audit

> **Implementation status:** T1.1–T1.4, T2.1–T2.3, T3.1, T3.2, T3.4, and T3.5 are
> implemented on this branch. Two amendments were made during review: `apply_layout`
> now fires `REDRAW` so the T1.2 cache picks up a `POST /layout` without waiting for a
> poll round-trip, and the T1.4 shared buffers are zeroed in place (avoiding a 7 KiB
> stack temporary — the hazard `framebuffers()` documents). Deliberately not done:
> **T1.5** (glyph cache — profile after T1.2 before adding cache complexity), **T2.4**
> (boot panics are acceptable as-is), **T3.3** (DNS packing dedup — low value).

A full-codebase audit of `firmware/` (~3,900 lines) focused on the known bottleneck:
memory and general resource consumption on the ESP32-S3. Every finding below cites the
code it comes from, and every Tier 1 finding was verified against the actual source
(including the esp-alloc 0.10.0 allocator internals) — none of it is speculation.

**The headline numbers:**

- Internal heap: **73,744 B** (`main.rs:53`, reclaimed bootloader RAM). This is the
  scarce resource — WiFi's DMA-capable allocations must come from it.
- Static/`.bss`/task-arena reservations identified: **~105 KB** (dominated by the two
  ~28 KB framebuffers, which are DMA-bound and must stay in internal RAM).
- **36,864 B of that internal heap — half of it — is occupied by buffers that were
  meant to live in PSRAM but don't** (T1.1 below). Fixing that one item roughly
  doubles free internal RAM.

---

## Tier 1 — Memory / resource wins

Ordered by RAM-freed-per-unit-risk. T1.1 is the big one.

### T1.1 — The poll task's TLS/HTTP buffers land in the internal heap, not PSRAM

**Problem.** `poll.rs:84-86` allocates the TLS read record (16 KiB), TLS write record
(4 KiB), and HTTP body buffer (16 KiB) — 36,864 B total, held for the life of the
process — with plain `vec![0u8; N]`:

```rust
let mut read_record = vec![0u8; TLS_READ_BUF];
let mut write_record = vec![0u8; TLS_WRITE_BUF];
let mut http_buf = vec![0u8; HTTP_BUF];
```

Two comments claim these live in PSRAM (`poll.rs:33` "the buffer lives in PSRAM";
`main.rs:52` "The large TLS/HTTP buffers live in PSRAM"). **They don't.** Verified
against esp-alloc 0.10.0: the global allocator is first-fit across regions in
registration order, and `main.rs:53-55` registers the internal heap first, PSRAM
second. Plain `vec![]` therefore serves these 36 KiB from the 73,744 B internal heap —
consuming ~50 % of it and competing directly with WiFi's DMA allocations, which is
exactly the pressure `LOW_HEAP_WARN_BYTES` / `warn_low_heap` (`httpd.rs:235-244`)
exists to catch.

**Fix.** Allocate explicitly in external memory, extending the pattern `shared.rs`
already uses for `CUSTOM_LAYOUT`/`PREVIEW_LAYOUT` (`Box::new_in(_, ExternalMemory)`).
`vec!` can't take an allocator, so:

```rust
use esp_alloc::ExternalMemory;
let mut read_record = Vec::with_capacity_in(TLS_READ_BUF, ExternalMemory);
read_record.resize(TLS_READ_BUF, 0u8);
// … same for write_record and http_buf
```

`#![feature(allocator_api)]` is already enabled (`lib.rs:2`) and esp-alloc's `nightly`
feature is already on (Cargo.toml), so no build changes are needed. The buffers deref
to `&mut [u8]`, so `fetch()`'s signatures are untouched. Also correct the comments at
`poll.rs:26-34` and `main.rs:51-52` to say the placement is now *enforced*, not assumed.

**Saving:** ~36 KiB internal heap reclaimed (≈ doubles free internal RAM).
**Risk: low.** These are byte scratch for embedded-tls/reqwless, not DMA targets;
PSRAM's higher latency is irrelevant at network speed. Blast radius: one task.

### T1.2 — Stop cloning the ~900 B `Layout` on every animation frame

**Problem.** In Custom UI mode with any scrolling (marquee) element, the render loop's
animating branch re-runs `draw_state` every `FRAME_MS` = 50 ms tick
(`display/mod.rs:275-282`), and each call reaches `custom_layout()`
(`display/mod.rs:324`), which clones the full ~900 B `Layout` out of a
`BlockingMutex` **critical section** (`shared.rs:86`) — up to 20 clones/second,
~18 KB/s of memcpy performed with interrupts disabled, indefinitely. The comment at
`shared.rs:47` ("the render task clones it out once per redraw, which is
event-driven — not per-DMA-frame") is inaccurate for this path. (The preview path has
the same shape but is transient — `preview_layout()` short-circuits via an atomic when
no preview is active.)

**Fix.** Clone once per layout *change*, not per frame. The layout only changes via
`apply_layout` (POST /layout) or `set_preview`/`end_preview` (POST /preview), all of
which already wake the render loop through the `DISPLAY`/`REDRAW` signals. Hold a
`let mut layout_cache: Option<Layout> = None;` local in `render_task`; refresh it when
the loop (re)enters and on the `Either3::Second`/`Either3::Third` select arms (state
change / `REDRAW`); leave it untouched on the frame-tick arm. Pass the cached layout
into `draw_state` by reference instead of having it call `custom_layout()` /
`preview_layout()` internally. Preserve the preview-overrides-persisted-mode
precedence (`display/mod.rs:312-329`) and the preview auto-revert on expiry
(`display/mod.rs:242-246`); then fix the `shared.rs:47` comment so it's true again.

**Saving:** ~20 clones/s → ~1 per change; removes recurring interrupt-disabled memcpy
from the render core. **Risk: moderate** — touches the render dispatch and
`draw_state`'s signature; the preview/custom precedence and expiry semantics must
survive. Do **not** touch the PSRAM boxing of the layout statics — this extends that
design, it doesn't replace it.

### T1.3 — `write_record` holds two 3 KB arrays on the stack at once

**Problem.** `storage.rs:135,139` keeps `payload: [u8; MAX_PAYLOAD]` (3,072 B) **and**
`buf: [u8; 8 + MAX_PAYLOAD + 4]` (3,084 B) live simultaneously — **6,156 B of stack**
per flash save, the largest single stack footprint in the firmware. It runs on core 0
(from HTTP handlers, `portal_wifi_task`, `button_task`) — the same stack the code's own
comments (`main.rs:100-110`, `poll.rs:51`) document as tight enough to have already
overflowed once.

**Fix.** Serialize directly into the payload region of the framed buffer and back-fill
the header — the header bytes (`0..8`) and payload region (`8..`) don't overlap:

```rust
let mut buf = [0u8; 8 + MAX_PAYLOAD + 4];
let len = serde_json_core::to_slice(value, &mut buf[8..8 + MAX_PAYLOAD]).map_err(|_| ())?;
buf[0..4].copy_from_slice(&magic.to_le_bytes());
buf[4..8].copy_from_slice(&(len as u32).to_le_bytes());
```

**Saving:** ~3 KiB off peak core-0 stack per save. On-disk record framing is
byte-identical. **Risk: low.** One function.

### T1.4 — Duplicate statics reserved for mutually-exclusive boot modes

**Problem.** Only one of `config_server_task` (`httpd.rs:296`) / `setup_server_task`
(`portal.rs:307`) ever runs per boot, but both task futures link into the binary, so
both copies of `serve()`'s local buffer set (`tcp_rx` 1024 + `tcp_tx` 4096 + `http_buf`
2048 = 7,168 B, `httpd.rs:282-284`) are always reserved — ~7 KiB permanently dead.
Similarly, the two `mk_static!(StackResources<8>, …)` call sites (`main.rs:149,174`)
each expand a fresh `static` (`lib.rs:44-47`), reserving two when one is ever used.

**Fix.** Hoist the buffers into a single shared `StaticCell` and `.take()` it inside
whichever server task actually spawns; same for one shared `StackResources<8>`. The
`.take()` panic-on-double-take enforces the "only one server per boot" invariant that
this change makes load-bearing — add a comment saying so. Don't try to merge the two
routers themselves (their `PathRouter` types differ); share only the buffers.

**Saving:** ~7 KiB `.bss` + one `StackResources<8>`. **Risk: moderate** — the
mutual-exclusivity invariant becomes load-bearing (enforced by `.take()`).

### T1.5 — (CPU, do after T1.2) Glyph cache for scaled custom text

`blit_scaled_text` (`display/custom.rs:103-129`) re-rasterizes every visible glyph
through embedded-graphics `Text::draw` into a fresh `GlyphCanvas` on every frame of a
scrolling custom-text element. Pure CPU cost (no allocation — the canvas is a ~60 B
stack array). Memoize the 1-bit `lit_pixels` per `(char, font)` in a small fixed-size
cache so only the blit runs per frame. Worthwhile only after T1.2; until then the
layout clone dominates the frame cost. **Risk: low–moderate**, isolated to `custom.rs`.

---

## Tier 2 — Robustness

### T2.1 — `scan_handler` can emit truncated (invalid) JSON exactly when it matters

`portal.rs:279` builds the scan-result list in a `String<768>` with every push result
discarded (`let _ = …`). `SCAN_CACHE` holds up to 16 networks (`portal.rs:53`) at
~55-90 B of JSON each, so a full cache (~1,120 B+) overflows 768 and the response is
cut mid-element with no closing `]` — the setup page's `JSON.parse` fails and the
network list appears *empty* precisely when the most APs are visible. Fix by building
entries in a loop that stops cleanly on the first failed push and always appends `]`
(preferred over enlarging the static, given the RAM budget).

### T2.2 — `get_config` silently truncates on future field growth

`httpd.rs:119` serializes the config into a `String<224>` and discards the `write!`
result. Today's payload fits (~150 B); the next added field silently truncates the
response into invalid JSON. Check the `write!` result (and name the capacity constant
with a comment tying it to the field set).

### T2.3 — Make the `JSON_TX_BUF` soundness invariant enforceable

The `static mut JSON_TX_BUF` pattern (`httpd.rs:148-173`) is a deliberate,
well-documented win (avoids ~20-30 KiB of picoserve future bloat) — keep it. But its
soundness rests entirely on the single-connection-at-a-time invariant of `serve()`.
Add a cheap `AtomicBool` reentrancy guard (debug-panic on concurrent entry) or at
minimum a `// SAFETY-INVARIANT:` marker at the `serve()` loop it depends on, so a
future move to concurrent connections can't silently break it.

### T2.4 — Boot-time panics (leave mostly as-is)

`main.rs:136` (`expect("wifi init")`), `display/mod.rs:184` (Hub75 init), and the
`.spawn().unwrap()` sites are genuinely unrecoverable init failures; panicking (→
esp-backtrace reset) is defensible. No action required; optionally convert spawn
failures to `log::error!` + reset for field diagnosability.

---

## Tier 3 — Readability / duplication

- **T3.1** UDP-socket boilerplate (`rx_meta`/`tx_meta`/buffers + `UdpSocket::new`) is
  repeated 4× (`mdns.rs:44-48`, `sntp.rs:26-30`, `portal.rs:169-173`,
  `portal.rs:212-216`). Extract a helper; the buffers must stay in the caller's scope
  (the socket borrows them), so it takes the arrays as `&mut` parameters.
- **T3.2** The read-modify-write save pattern is repeated 4× in `storage.rs:181-214`
  (`save_wifi`/`save_selection`/`save_config`/`save_layout`). Collapse into
  `fn update(&mut self, f: impl FnOnce(&mut Persisted)) -> Result<(), ()>`.
- **T3.3** DNS answer-record byte-packing exists in both `mdns.rs:154-179` and
  `portal.rs:242-260`. The wire formats differ (mDNS announce vs unicast catch-all);
  share only the answer-record writer if it reads cleanly — low value otherwise.
- **T3.4** The two `\uXXXX`-unescape scratch sizes (`poll.rs:245` `[u8;96]` vs
  `storage.rs:119` `[u8;256]`) serve the same 64-byte worst-case field with different
  margins. Reconcile under one documented rule (scratch ≥ longest decoded field).
- **T3.5** Split `main()`'s two boot arms (`main.rs:145-190`) into
  `boot_provisioning()` / `boot_connected()`; name the inline TTLs (`mdns.rs:175`
  `0x78`, `portal.rs:257` `60`) and response-buffer capacities (`httpd.rs:119`,
  `portal.rs:279`).

### Deliberately not proposed

- **`parse_departures`' ~5-6 KB transient stack** (`poll.rs:242-282`: `Board`'s
  `Vec<Entry, 20>` + `matches`) is a known, already-tuned constraint (commit
  `78c5126`). `serde_json_core` returns the board by value; routing it to PSRAM isn't
  trivially safe. Revisit only if core-0 stack pressure persists after T1.3/T1.4.
- **Framebuffers stay in internal RAM.** HUB75 via LCD_CAM is DMA-driven; the two
  ~28 KB framebuffers (`display/mod.rs:337-363`) must remain DMA-capable. Their
  in-place `.bss` construction fixed a real boot-stack overflow — don't touch it.

## Existing good patterns (preserve)

- `shared.rs` PSRAM boxing (`Box::new_in(_, ExternalMemory)`) for the layout mirrors —
  T1.1 extends this to the poll buffers.
- `httpd.rs` borrowed-`Body`/`raw_json` static-buffer response pattern (T2.3 only
  hardens its invariant).
- `storage.rs` `[magic][len][payload]` framing with readback verification and the
  single-read `load_boot` (T1.3 keeps framing byte-identical).
- `model.rs` `Layout::sanitize()` server-side clamping of all client input.

## Interactions to keep in mind

- **T1.1 changes the headroom math everything else was tuned against.** The low-heap
  warning threshold (`LOW_HEAP_WARN_BYTES`), the small `serve()` buffers, the tight
  `APP_CORE_STACK`, and the `JSON_TX_BUF` trade-off were all sized against a
  half-consumed internal heap. They all remain correct and cheap after T1.1 — keep
  them — but re-baseline `LOW_HEAP_WARN_BYTES` against the new free floor.
- **T1.3 + T1.4 both relieve the same core-0 stack budget** that the deliberately-small
  buffers and the `parse_departures` tuning defend.
- **T1.2 is a prerequisite for T1.5 mattering**, and makes the `shared.rs:47` comment
  true again.

## Suggested implementation order

1. **T1.1** (biggest win, lowest risk, smallest diff)
2. **T1.3** (small, isolated, immediate stack relief)
3. **T2.1 / T2.2** (small correctness fixes)
4. **T1.2** (biggest CPU/latency win; needs care around preview semantics)
5. **T1.4** (moderate; makes an invariant load-bearing)
6. **T2.3, T3.x, T1.5** (hardening + cleanups, any order)
