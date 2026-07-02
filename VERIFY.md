# VERIFY.md — UI Builder acceptance gates & frozen contract

Working doc for the UI-builder build (`FEATURE_UI_BUILDER.md`). Two gate types per milestone:
- **Auto gate** — a command that exits 0/1; the real self-driving loop.
- **HW gate** — needs the flashed ESP32 + eyeball; handed to the user (panel is reachable this build).

Kickoff decisions (2026-07-02): panel reachable as we go; simulator↔panel fidelity bar = **glyph-for-glyph identical**.

Auto gate commands (run in `firmware/`, after `. ~/export-esp.sh` for the xtensa linker):
```
cargo build --release          # must finish clean
cargo clippy --release         # must add no NEW warnings vs baseline
```
Baseline clippy already emits ONE pre-existing warning (`display.rs:1039`, the `framebuffers()`
mutable-borrow) and the project does NOT compile clean under `-- -D warnings` (pre-existing
`result_unit_err` on `save_wifi`/`save_config` etc.), so the gate is "no new warnings", not `-D warnings`.
New storage fns mirror the existing `Result<(), ()>` style deliberately.
The firmware crate is `no_std` + `xtensa-esp32s3-none-elf`, so host `cargo test` can't build it. Schema
round-trip / byte-budget / clamp-fuzz checks are covered by a host-target unit-test shim over the
schema types (see M1) that does NOT pull in `esp-hal` — run with its own `--target` (host).

---

## Frozen shared contract (Opus owns; Fable M4/M5 mirror verbatim)

Derived from the real firmware (`display.rs`, `model.rs`, `storage.rs`). Do not re-decide these.

### Palette (exact RGB from `display.rs`)
- `AMBER`  = `#FFA800` (255,168,0)   — preset `c=0`, primary text
- `ACCENT` = `#AA4A10` (170,74,16)   — preset `c=1`, copper / structure
- `DIM`    = `#744A1E` (116,74,30)   — preset `c=2`, secondary
- `OFF`    = `#000000` — badge cut-outs only, not a user colour.
- `elem_color(el)`: if `col` present → `#RRGGBB` from `col & 0xFFFFFF`; else preset for `c` (0→AMBER,1→ACCENT,2→DIM).
- Every colour passes the brightness `scaled()` choke point on the firmware. The simulator draws at
  full strength (phone screen); the *panel* is the brightness-truth surface.

### Coordinate system
- Origin top-left `(0,0)`; x→right, y→down; valid `0..=63`.
- `y` is **baseline-top** for text (embedded-graphics `Baseline::Top`).

### Fonts (ISO-8859-1 mono, from `embedded_graphics::mono_font::iso_8859_1`)
- `s=0` → `FONT_5X7`: glyph 5×7, **advance 5** px.
- `s=1` → `FONT_6X10`: glyph 6×10, **advance 6** px.
- Scale `k ∈ {1,2,3}`: pixel-doubling blitter, each source pixel → `k×k` block.
  Scaled advance = `char_w × k`; scaled height = `font_h × k`.
- Marquee: fits if `text_w ≤ avail` (draw flush); else scroll. `GAP=14` px between wrapped copies,
  `HOLD_FRAMES=100` initial pause, 1 px/frame, `period = text_w + GAP`, `FRAME_MS=50`.
- Badge (`draw_badge`): width = `line.chars * 6 + 5`, height `11`; label at `(x+3, y+1)` in FONT_6X10,
  fill background with text cut out.

### Schema (compact JSON keys — see §5.3/§5.4)
- Top: `{ "v":1, "e":[ …elements ] }`. Empty `e` = "no custom layout".
- Element flat struct, numeric `t` tag, all non-`t/x/y` fields `#[serde(default)]`:
  - `t` type: 0=Text 1=Departure-field 2=Station 3=Clock 4=Date 5=Divider 6=Icon
  - `x`,`y` (0..=63), `w` (clip/marquee width or divider length), `s` (font 0/1), `k` (scale 1..=3),
    `c` (preset 0..=2), `col` (optional u32 0xRRGGBB, overrides `c`, omitted when unset),
    `a` (align 0=L/1=C/2=R), `v` (Text literal `String<24>`),
    `di` (dep slot 0..=2), `fk` (field 0=badge/1=direction/2=time), `sp` (split bool; firmware ignores),
    `th` (divider thickness 1..=2), `f` (clock/date format), `g` (icon glyph 0/1/2).
- Departure = three `t=1` sharing `di`, one per `fk`. ≤3 departures → ≤9 `t=1` elements.
  Default dep colours: badge & direction `c=0` (amber), time `c=1` (copper).

### Element render specifics (M2 froze these; Fable M4 simulator mirrors verbatim)
- **Text scaling path.** Every text-bearing element (Text, Departure direction & plain-text badge,
  Station, Clock, Date, Departure time) renders through one blitter: draw each glyph from the mono
  font (`s`) once, then pixel-double each source pixel to a `k×k` block. Advance = `char_w×k`
  (`char_w` 5/6). Fit → flush, aligned within the box `w` (`a`=0/1/2); overflow → marquee (only for
  Text/Direction/Station), `GAP=14`, `HOLD_FRAMES=100`, 1 px/frame, clipped to `[x, x+w)`. `w=0` =
  natural width, unbounded, no clip, `a` ignored (flush at `x`).
- **Badge (`t=1 fk=0`).** `line_badges_enabled()` → `draw_badge` (fixed 11 px, FONT_6X10, `k`
  ignored), fill = `elem_color`, digits cut out (OFF). Else → plain text via the scaling path (`s`/`k`).
- **Time (`t=1 fk=2`).** `Some(0)` (now) → `draw_train_front` pictogram scaled by `k` (9×10 base);
  else `N'`/`--` text via the scaling path.
- **Divider (`t=5`).** Horizontal bar at `y`, length = `w` (or `COLS−x` when `w=0`), thickness `th`
  (1..=2), `elem_color`. Rectangle fill.
- **Clock (`t=3`).** Local Swiss time. `f=1` → `H:MM` (no leading zero); else → `HH:MM`. Static
  (no marquee, clips to `w`). Draws nothing until SNTP has synced.
- **Date (`t=4`).** Local Swiss date. `f=1` → `DD.MM.YYYY`; else → `DD.MM.` (trailing dot). Static.
- **Icon (`t=6`).** `g=0` tram-front (9×10, `draw_train_front` glyph), `g=1` Z-blind (3×5, the
  connecting-animation "Z"), `g=2` arrow (7×5). Each pixel-doubled by `k`, `elem_color`.
- **Missing data.** A Departure field whose slot `di` has no live departure draws nothing. An empty
  layout (`e=[]`) in Custom mode falls back to the Default board.

### Bounds (enforced BOTH phone + firmware — §5.5)
- `LAYOUT_MAX_BYTES = 1536` (authoritative; serialized JSON must be ≤). `MAX_ELEMENTS = 16` (sanity).
- Text `v` = `String<24>`. Clamp `x,y,w ∈ 0..=64`, `k∈1..=3`, `c∈0..=2`, `a∈0..=2`, `di∈0..=2`, `fk∈0..=2`, `th∈1..=2`.
- Firmware clamps/skips out-of-range & off-panel; never panics. Unknown `t`/fields ignored; newer `v` → treat as no layout.

### UI mode (replaces `focusView` bool)
- `Config.ui_mode` (JSON `uiMode`) u8: `0=Default`, `1=Focus`, `2=Custom`. `#[serde(default)]`→0.
- Migration: old `focusView:false→0`, `focusView:true→1`. Sibling `offWhenDimmed` untouched.
- Not a new endpoint — flows through existing `/config`. `get_config` `write!` token: `"uiMode":{}`.

### Endpoints (§7.4)
- `GET /layout` → persisted layout JSON (or `{"v":1,"e":[]}`).
- `POST /layout` (`Json<Layout>`) → validate/clamp, reject > LAYOUT_MAX_BYTES, persist, apply, signal redraw.
- `POST /preview` (`Json<Layout>`) → transient mirror, no flash, arm ~15s auto-revert, signal redraw.
- `POST /preview/end` → drop transient, re-render persisted mode+layout.
- Empty-`e` `POST /layout` = clear; page pairs with `POST /config uiMode=0`.

---

## M1 — UI-mode rework + storage plumbing  ✅ code + auto gates green
- **Auto (PASS):** `cargo build --release` clean; `cargo clippy` adds no new warnings; host schema shim
  (`scratchpad/schema-shim`, `#[path]`-includes the real `model.rs`) passes 5 gates:
  round-trip byte-identity (incl. accented + custom `col`), `focusView`→`uiMode` migration,
  `sanitize()` range clamping, parse-time bounds (>MAX_ELEMENTS / over-length text rejected),
  and byte budget.
- **Budget finding:** serde-json-core 0.6's *serializer* emits **raw UTF-8**, NOT the 6-byte `\uXXXX`
  escapes §6 feared. So the schema's own bounds (16 elem × `String<24>`) cap the absolute worst-case
  *valid* layout at **1465 B < LAYOUT_MAX_BYTES(1536)** — the firmware byte-cap check is a pure
  backstop a well-formed layout never trips. Whole record worst case ~2665 B < MAX_PAYLOAD(3072) < 4088.
  (`from_slice_escaped` on read still handles escapes if a *client* sends them.)
- **HW (PENDING user re-flash):** `uiMode` round-trips via `/config`; a hand-POSTed layout persists &
  reloads across reboot; panel switches Default/Focus/Custom by mode (Custom w/ no layout → Default board).
## M0 — Firmware memory-budget rework (the real blocker; resolved by static analysis)

**Symptom:** baseline rock-solid, but *any* M1 firmware code deterministically corrupted the WiFi
blob (LoadProhibited/StoreProhibited at boot's first network I/O). Not the layout logic — the layout
`Box` never even runs on a device with no saved layout.

**DRAM map (ESP32-S3, `size -A` + nm), why it's fragile:** the cpu0 main stack is wedged between
`.bss` (floor = `_bss_end` = `_stack_end_cpu0`) and the reclaimed heap (`dram2_uninit`, **fixed** at
`0x3FCDB700` = `_stack_start_cpu0`). So the stack size = `heap_start − _bss_end`; **any `.bss` growth
shrinks the cpu0 stack**, and the poll task's TLS handshake + stationboard JSON parse run on that
stack already near its limit (poll.rs / commit 78c5126). Baseline cpu0 stack = **81 KB**.

**Root cause (measured, not guessed):** M1's `.bss` grew **+30 KB**, all in the
`config_server_task` embassy arena (20 KB → 50 KB). The arena holds the task's future; picoserve's
response state machine keeps **~20 by-value copies** of the response `Content`. `get_layout`
returning `OwnedJson<1536>` (owned `String`) or `response::Json<Layout>` (serde-serialized into an
async writer) put ~1.5 KB × 20 ≈ 30 KB into the future → stack 81 KB → 51 KB → overflow into `.bss`
where WiFi state lives → corruption. (Adding 8 KB stack only *moved* the crash; reducing my `.bss`
back is the real fix.)

**Fix:** serve the layout as a **borrowed byte slice** — `RawJson(&'static [u8])` holds only a fat
pointer (16 B), so picoserve's ~20× copies cost ~320 B, like the 25 KB `&'static str` index page
that costs nothing. `get_layout` serializes into a `static` TX buffer (safe: `config_server_task`
serves one connection at a time) and hands out a slice. Result: arena back to **20.5 KB** (+368 B),
cpu0 stack **~79 KB** (baseline 81 KB). The `Json<Layout>` *extractor* on POST is cheap (~0).

**RULES for M2+ (avoid re-bloating the cpu0 stack):**
- HTTP responses larger than a few hundred bytes → **borrowed** `Content` (`&'static`/`RawJson`),
  never owned `String`/`OwnedJson<N>` or `response::Json` of a big/deep type. The multiplier is ~20×.
- Large runtime state (layout mirror) → **PSRAM** via `Box::new_in(_, esp_alloc::ExternalMemory)`
  (needs `#![feature(allocator_api)]` + esp-alloc `nightly`), never `.bss` (stack floor) and never
  the default/internal heap (WiFi DMA RAM).
- Watch the `config_server_task` arena (`nm --print-size … | grep config_server_task4POOL`) and
  `_stack_end_cpu0` after any httpd change; keep cpu0 stack ≳ 75 KB.

## M2 — Firmware renderer + scaling  ✅ code + auto gates green
- **Auto (PASS):** `cargo build --release` clean; `cargo clippy --release` adds **no new warnings**
  (only display.rs finding is the pre-existing `framebuffers` `mut_from_ref`, now at line ~1378
  after +230 lines; the other 9 are the untouched model/storage/portal/wifi baseline). None of the
  new M2 symbols (`draw_custom_layout`, `blit_scaled_text`, `place_text`, `GlyphCanvas`,
  `draw_dep_field`, `elem_color`, `draw_clock/date/divider/icon`, `blit_bitmap`) are flagged.
- **Design notes:** glyph scaling renders each font glyph once into a 6×10 `GlyphCanvas`
  (embedded-graphics' own rasteriser) then pixel-doubles to `k×k` — NOT `ImageRaw::GetPixel`, which
  is O(atlas) per pixel via `nth()` and too slow for a 20 fps marquee. Render specifics (clock/date
  formats, icon glyphs, badge/time behaviour) are frozen in the contract above for the M4 simulator.
  Dispatch is on `ui_mode()` only (M3 will add the preview-forces-custom branch).
- **HW (PENDING user re-flash):** Custom-mode POSTed layout draws every element type; both fonts +
  `k∈{1,2,3}` scale correctly; departure fields resolve to the right live slot; a missing slot draws
  nothing; empty layout → Default board.

## M3 — Live-preview endpoints + watchdog  ✅ code + auto gates green
- **Auto (PASS):** `cargo build --release` clean; `cargo clippy --release` adds **no new warnings**
  (baseline set unchanged; the one I introduced, `collapsible_if` on the deadline check, was folded
  into a let-chain). Memory re-measured per M0 rules: `config_server_task` arena **21,280 B**
  (~20.8 KB, +288 B vs M1 for the two handlers — far from the +30 KB that crashed WiFi); cpu0 stack
  `_stack_start_cpu0 − _stack_end_cpu0` = **~76.6 KB** (> the ≳75 KB floor). New `.bss` beyond the
  arena ≈ 50 B (REDRAW signal + PREVIEW_DEADLINE atomic + PREVIEW_LAYOUT pointer).
- **Design:** NO separate watchdog task (a new task = new `.bss` TaskStorage → shrinks cpu0 stack).
  The auto-revert is folded into `render_task`: a top-of-loop expiry check calls `end_preview()`, and
  the idle `select3` sleeps until `min(preview_deadline, brightness-refresh)` so it wakes in time.
  `POST /preview` uses a new `REDRAW` signal (render redraws the *current* `DisplayState`, no network
  re-poll — unlike `SELECTION_CHANGED`) so high-frequency editor pushes stay cheap. Transient
  `PREVIEW_LAYOUT` mirror is PSRAM-boxed and separate from `CUSTOM_LAYOUT`, so ending a preview
  restores the persisted layout with no re-fetch. `POST /layout` also clears preview state (§7.4).
- **HW (PENDING user re-flash):** `POST /preview` shows on the panel regardless of `uiMode` and
  writes NO flash (reboot → `GET /layout` still returns the pre-preview saved layout); `POST
  /preview/end` reverts immediately; stopping pushes reverts after ~15 s.

## M4 — Simulator + thumbnail  ✅ code + auto checks green (device parity PENDING)
- **Split:** Opus wrote the parity-critical render ENGINE; a Fable subagent wrote the presentation.
- **Engine (Opus, verified):** fonts extracted from embedded-graphics' OWN rasteriser via a host tool
  (`scratchpad/fontgen`, uses `iso_8859_1::{FONT_5X7,FONT_6X10}`) → `FONTS` JS table, so glyphs are
  identical-by-construction to the firmware. Faithful JS port of `display.rs` custom renderer
  (`blitScaledText`/`placeText`/dispatch/clock-date/icons/badge) locked in `web/index.html`
  `<script id="panel-engine">`, exposed as `window.ZugliPanel.renderLayout(layout, ctx)` →
  `{grid:Uint32Array(64*64), animating}` (cell 0=off, else full-strength 0xRRGGBB). jsc-verified vs
  the M2 layout: renders every element type correctly (checksum-stable across edits).
- **Presentation (Fable, Opus-reviewed):** `createPanelSim(canvas)` LED-dot renderer (dpr-scaled
  backing store, cached substrate, glow 0.46 < 0.5 pitch so dots stay discrete), rAF loop advancing
  `ctx.frame` by 1 per 50 ms (firmware tick), `prefers-reduced-motion` = static frame,
  `renderPanelThumbnail`, `window.SAMPLE_PANEL_CTX`, and a `#simdemo` dev harness. Reviewed:
  engine block byte-unchanged, `<script>` tags balanced, panel-sim parses clean, no em-dash in new
  UI copy, `#0a0a0a` (not `#000`), 44 px touch target, ARIA on dialog+canvas.
- **HW (PENDING):** open `http://<host>/#simdemo` on the phone and compare to the panel showing the
  same M2 layout (`./test-m2-layout.sh <host>`) — must match **glyph-for-glyph** for every element
  type & scale, marquees scrolling in step. (Clock/Date differ only in value, not glyph shape:
  phone uses browser time, panel uses device SNTP time; the FORMAT is identical.)

## M5 — Main-page selector + editor  ✅ code + auto gates green (device UX PENDING)
- **Split:** Fable subagent wrote the editor UI; Opus provided verbatim logic helpers
  (`serializeLayout`/`clampElement`/`makeElement`+`makeDeparture`/`createPreviewDriver`) and reviewed.
- **Auto (PASS):** (1) engine + sim blocks byte-identical to the pre-M5 backup (checksum 882/670374748,
  `panel-engine`+`panel-sim` hash-equal); (2) `focusView`/`focusToggle`/`opt-focus` fully removed,
  `cfg.uiMode:0` in place, `renderConfig` reflects the selector + calls `zugliConfigChanged`;
  (3) new `<script id="ui-builder">` parses clean; (4) **whole-page JS smoke test** under a stubbed
  DOM (`window===globalThis`) — all 4 script blocks initialize without throwing (no load-time
  ReferenceError); (5) helpers pasted verbatim; endpoints correct (`GET/POST /layout`, `/preview`,
  `/preview/end`, `/config uiMode`); `<script>` tags balanced 4/4.
- **Verified wiring:** selector → `applyChange(cfg.uiMode=i)`, Custom-with-empty opens editor;
  `driver = createPreviewDriver(()=>working)`, `mutated()` → dirty+enable Save+`driver.edit()`;
  Save → byte guard(≤1536) → `POST /layout` (8s abort) → `uiMode=2` → `driver.close()` → thumbnail;
  Clear → empty `POST /layout` → `uiMode=0`; Cancel → discard-confirm-if-dirty → `driver.close()`.
- **HW (§10.5 full flow, PENDING device):** switch modes from main page; add ≤3 departures; split a
  departure (double-tap/button) → fields move independently & can't reconnect; per-field colour
  (presets + native custom picker) & scale; design → live panel mirror (debounced `/preview` + 5s
  keepalive + ~15s auto-revert) → Save persists as Custom; Cancel/abandon reverts; Clear → Default;
  nudge ±1 LED; corner-handle resize; "Layout full" at 16 elems / >budget. Editor served from the
  device (network calls fail soft off-device).
