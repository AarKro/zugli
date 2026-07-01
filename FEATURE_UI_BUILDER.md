# Feature Brief — Custom Board Layout Builder ("UI Builder")

> Status: proposed / not yet implemented. This document is the implementation brief for an
> agent to pick up. It assumes the codebase at the point where §4 (config page), §7.7
> (display rendering) and §7.8 (flash persistence) of `PROJECT_BRIEF.md` are already built.
> Section numbers below prefixed `PB` refer to `PROJECT_BRIEF.md`.

---

## 1. Summary

Add an on-phone **layout builder** to the Zügli config page. The user gets a simulated
**64×64 LED panel** on screen and can **add**, **move**, and **resize/scale** elements
(text, clock, the live departures block, dividers, icons…) to design their own board.
Exactly **one** custom layout is stored at a time (no versioning, no history). When a custom
layout exists the firmware renders it in place of the built-in departures board (PB §7.7);
when none exists, the built-in board is used unchanged. The editor loads the saved custom
layout if present, otherwise starts from an empty canvas.

The whole feature must be **phone-first**: the editor is a full-screen, touch-driven surface
served from the same self-contained `web/index.html`, and the on-screen simulator must be a
faithful, pixel-accurate preview of what the physical panel will draw.

---

## 2. Business case

- **Differentiation & delight.** A DIY LED departure board is already a maker product; the
  ability to *design your own board face* turns a fixed appliance into a personal object.
  It is the single most shareable, "show your friends" feature the product can add without
  new hardware.
- **Fits the existing product promise.** Zügli already lets the user tune *what* it tracks
  (stop, connections) and *how* it renders (hide-city, badges, brightness — PB §4.6). A
  layout builder is the natural apex of that "make it yours" ladder: it tunes *where and how
  big* everything sits.
- **No recurring cost, no backend.** Everything runs on the phone and the ESP32 over the
  existing same-origin HTTP channel (PB §4). No servers, accounts, or cloud — consistent
  with the project's "self-contained, offline-capable device" principle.
- **Low blast radius.** The custom layout only governs the **running departures screen**.
  Provisioning, connecting, idle-address and offline states keep their built-in rendering,
  so a badly-designed layout can never lock the user out of setup or recovery.
- **Reversible by design.** One-tap "Reset to default" restores the built-in board, so the
  feature is safe to ship: worst case, the user reverts.

### Target user & job-to-be-done
"I've mounted Zügli on my shelf. I want the stop name bigger, a clock in the corner, and only
two departure rows — laid out the way *I* want, from my phone, in under two minutes, without
flashing new firmware."

---

## 3. Goals and non-goals

### Goals
- A full-screen, touch-first layout editor reachable from the config page.
- A **pixel-accurate** 64×64 simulator that mirrors the firmware's fonts, palette, and
  coordinate system.
- **Live on-panel preview:** the physical panel mirrors the working layout in real time from
  the moment the editor opens, so the phone simulator and the device stay in lock-step while
  the user designs.
- Add / move / resize / delete elements; edit per-element properties; nudge for pixel
  precision; reset to default.
- A **compact, versioned** layout schema that round-trips phone ⇄ flash ⇄ firmware renderer.
- Firmware renders the custom layout live on `Save`, no reboot (mirrors PB §4.4 behaviour).

### Non-goals (v1)
- **No history / undo stack across sessions.** One layout stored. (A single in-editor undo
  step is optional; see §4.6.)
- **No multiple layouts / profiles / scheduling.** One layout only.
- **No re-theming the non-departures states** (provisioning, connecting, idle, offline).
- **No free-form pixel art / per-pixel painting.** Elements only.
- **No custom fonts or colours** beyond the fixed brand palette and the two firmware fonts
  (plus integer upscaling). See §5.4.
- **No multiple concurrent editors.** The live preview is single-owner: one phone drives the
  panel at a time (§4.3).

---

## 4. UX design

The config page today is a single centered column (PB §4.1) with a tracking-mode switch, a
stop search, a connection list, a `Save` button, and a **settings bottom-sheet** behind a
gear icon. The builder is added as a **full-screen overlay editor**, launched from a new,
discoverable entry point, and returning the user to the same page on close.

### 4.1 Entry point (recommended: a labelled card on the main page)

**Recommendation:** add a full-width **secondary card/button** on the main config page,
directly beneath the `save-section`, labelled **"Design your board →"** with a small **live
thumbnail** of the current layout (a scaled-down, non-interactive render of the simulator) on
its left. If no custom layout exists, the thumbnail shows the built-in board and the caption
reads "Default layout"; if one exists, the caption reads "Custom layout".

Rationale (industry standard for this class of feature — cf. LaMetric, Divoom Pixoo,
smart-display "design" tabs):
- **Discoverability.** A layout designer is a flagship feature; burying it in the settings
  sheet (which is for small display toggles) hides it. A thumbnail communicates current
  state *and* invites editing in one glance — the standard "edit your design" affordance.
- **Separation of concerns.** The gear sheet stays about *display toggles*; the card is about
  *composition*. This keeps the main flow (pick a stop → save) uncluttered.

The card is a distinct visual weight from the primary `Save to Zügli` button (secondary
styling: surface background, copper text/outline) so it never competes with the primary save
action.

> Alternative considered: a row inside the settings sheet ("Board layout ›"). Rejected as
> primary because it under-sells the feature and the sheet is modal-over-modal awkward on a
> phone. It may additionally be offered there for users who look in Settings first.

### 4.2 Editor screen anatomy (full-screen overlay)

Opens as a full-viewport overlay (same overlay/z-index machinery as the settings sheet, but
`inset:0`, not a bottom sheet). Top-to-bottom:

1. **App bar** (fixed, respects `env(safe-area-inset-top)`):
   - Left: **Cancel** (`✕`) — discards unsaved changes (confirm only if dirty).
   - Center: title **"Board layout"**.
   - Right: **Save** (primary copper button, disabled until the layout is *dirty*).
2. **Canvas** (the simulator): the 64×64 panel centered, scaled to `floor(min(availWidth,
   availHeight-chrome)/64)` px per LED (target 5–6 px/LED → 320–384 px). Rendered on a
   `<canvas>` as rounded "LED dots" on near-black, matching the panel look. A faint 1-LED
   grid overlay aids placement. `touch-action:none` so dragging never scrolls the page.
3. **Selection chrome** (when an element is selected): a bounding box drawn *around* the
   element in a contrasting outline, with **four corner resize handles**. Handles are drawn
   small but have an enlarged invisible hit area (≥ 32–44 px) so they're tappable on a phone.
4. **Bottom bar / palette:**
   - When **nothing** is selected: a horizontally-scrollable **element palette** (chips:
     Text, Departures, Clock, Date, Station, Divider, Icon) plus a persistent **"+ Add"**
     affordance. Tapping a chip adds that element (see §4.4).
   - When **an element is selected**: a **properties sheet** (see §4.5) with a **Delete**
     button, replacing the palette until deselected.

**Empty state:** canvas shows only the grid with centered hint text "Tap **+** to add your
first element." (Rendered as page HTML over the canvas, not on the simulated panel.)

**Live vs. sample data (phone simulator):** the simulator renders with **live** data where
possible — if a stop is already selected on the config page, reuse the page's existing
`stationboard` fetch (PB §6.2) to show real upcoming departures, the real station name, and
the real current time. If no stop is selected yet (or the fetch fails), fall back to
**representative sample data** (`"Zürich, Hauptbahnhof"`, three plausible departures, current
clock) so every element type previews meaningfully. The **on-panel** preview (§4.3) always
uses the device's real runtime data, since it renders through the normal departures pipeline.

### 4.3 Live on-panel preview

The physical panel mirrors the working layout **in real time**, starting the instant the
editor opens — the panel is the second, authoritative preview surface alongside the phone
simulator.

- **On open:** the editor loads the persisted layout (`GET /layout`) into its working copy and
  immediately pushes it to the device (`POST /preview`, §7.4), so the panel switches to the
  working layout before the user makes a single edit.
- **On every edit:** move / resize / add / delete / property change re-pushes the working copy
  via a **debounced** `POST /preview` (~150–250 ms) so the panel tracks the design without
  flooding the device with a request per drag-pixel.
- **Transient, never persisted.** `/preview` updates the device's *live* layout mirror only;
  nothing is written to flash until **Save** (§4.6). The panel shows the design; a reboot or a
  timeout (below) reverts to the last saved layout.
- **Idle keepalive + auto-revert safety.** While the editor is open the page sends a keepalive
  `POST /preview` (reusing the latest working copy) every ~5 s. The firmware arms an
  **auto-revert timer** (~15 s) on each preview push; if it expires without a new push — the
  phone locked, lost WiFi, or the tab was closed — the device reloads the persisted layout and
  leaves preview mode, so the panel can never get stuck showing an abandoned draft.
- **On close (Save or Cancel):** the editor ends preview explicitly (`POST /preview/end`,
  §7.4). After a **Save** the persisted and preview layouts are identical, so the panel is
  already correct; after a **Cancel** the device reverts to the saved layout.

### 4.4 Adding elements

- Tap a palette chip (or **+ Add** → a small type sheet). The new element is inserted at a
  sensible default position/size for its type (e.g. Text at `(2, 2)` size S; Departures block
  filling the lower panel) and is **auto-selected**, opening its properties sheet.
- Elements are placed into the layout array in insertion order; draw order = array order
  (later = on top). Overlap is rare at 64×64; a simple "Send to back / Bring to front" pair
  in the properties sheet is optional.
- Enforce the **`LAYOUT_MAX_BYTES`** budget live (see §5.5 / §6): the editor tracks the
  working layout's serialized size and, once the next element would cross the budget (or
  `MAX_ELEMENTS` is reached), disables the palette chips with a short "Layout full" note.
  Because a long accented Text string weighs far more than, say, a Divider, the trigger is the
  byte budget, not a fixed element count.

### 4.5 Editing an element (move / resize / properties / nudge / delete)

- **Move:** drag the element body. Position snaps to the **1-LED grid** and is **clamped**
  so the element stays fully within `0..63` on both axes.
- **Resize / scale / extend:** drag a corner handle. Semantics are per type (§5.4):
  - Text/Clock/Date/Station: resizing steps the **font size** (S → M → L…) and, horizontally,
    the **clip/marquee width**.
  - Departures block: resizing changes **width** and **row count / row height**.
  - Divider: horizontal drag changes **length**; vertical is fixed (1–2 px thickness prop).
  - Icon: resizing steps the integer **scale** (1×/2×/3×).
- **Nudge (critical for phones):** the properties sheet exposes **± arrow buttons** and
  **numeric x / y / w / h fields**. On a 5-px grid, dragging alone is too imprecise; the
  nudge buttons move by exactly 1 LED and are the accessible, pixel-perfect path. This is a
  first-class control, not an afterthought.
- **Properties sheet** (bottom sheet, per selected element): shows only the props relevant to
  the element's type (§5.4). Common controls: **colour swatches** (fixed palette — amber /
  copper / dim), **alignment** (left/center/right), **font size** (S/M/L), plus the
  x/y/w/h + nudges. Type-specific controls: Text → text input; Departures → row count,
  show-badge / show-destination / show-time toggles, hide-city toggle; Clock/Date → format;
  Icon → which glyph.
- **Delete:** a **trash** button in the properties sheet (with an inline confirm on a phone —
  a second tap or a small confirm chip). Deselecting (tap empty canvas) returns to the
  palette.
- **Deselect:** tap empty canvas area.

### 4.6 Save / cancel / dirty tracking

- The editor keeps a working copy of the layout array. Any mutation marks it **dirty** and
  enables **Save**.
- **Save:** `POST /layout` with the serialized layout (§5). Optimistic UX identical to the
  existing `/config` flow (`applyChange` in `web/index.html`): show "Saved — Zügli is
  updating." on success; on failure keep the editor open, surface "Couldn't reach Zügli — try
  again.", and do not close. On success, update the main-page thumbnail and close the editor.
- **Cancel:** if dirty, confirm ("Discard changes?"); otherwise close immediately.
- **Optional single-step in-editor undo:** a lightweight `Ctrl-Z`-style "Undo" affordance that
  reverts the last edit *within the current editing session only*. This is not persisted
  history and does not contradict the "one layout, no history" storage rule. Ship only if
  cheap; otherwise omit.

### 4.7 Reset to default

- A **"Reset to default"** action lives in the editor (e.g. an overflow item in the app bar,
  or a footer button under the palette). It clears the custom layout: `POST /layout` with an
  **empty layout** (or `DELETE /layout`, see §7.4), after a confirm dialog. The firmware then
  falls back to the built-in board (PB §7.7). The main-page thumbnail reverts to "Default
  layout".

### 4.8 Relationship to existing display settings (important)

The settings sheet (PB §4.6) has **Hide city names** and **Line badges** toggles that shape
the built-in board. With a custom layout active, the **departures element owns those choices
per-element** (its own `stripCity` / `showBadge` props). Decision:

- When a **custom layout is active**, the global *Hide city names* and *Line badges* toggles
  apply **only to the built-in fallback**, not to the custom board. To avoid confusion, show
  a one-line note in the settings sheet when a custom layout exists: *"A custom layout is
  active — these apply to the default layout."*
- **Brightness / auto-dim** are global and **always apply** (they scale the whole palette at
  the render choke point — PB §7.7 / `display::scaled`), custom layout or not.

---

## 5. Data model & layout schema

### 5.1 Design constraints driving the schema
- Must serialize compactly (flash budget — §6) with `serde-json-core`.
- Must deserialize on `no_std` with `serde-json-core`, which has **weak support for tagged
  Rust enums**. → Model each element as a **flat struct with a numeric type tag** and
  `#[serde(default)]` optional fields, **not** a Rust `enum` with data-carrying variants.
- Must be **versioned** so future element types / fields can be added without breaking old
  saved layouts (mirror the existing `#[serde(default)]` forward-compat approach in
  `model.rs` / `storage.rs`).

### 5.2 Coordinate system, palette, fonts
- **Origin** top-left `(0,0)`; x→right, y→down; valid range `0..=63`. `y` is the **baseline-
  top** for text (matches `display::left` using `Baseline::Top`).
- **Palette** (indices map to existing `display.rs` constants, scaled by brightness at draw):
  `0 = AMBER`, `1 = ACCENT` (copper), `2 = DIM`. (`OFF` is only for badge cut-outs, not a
  user colour.)
- **Fonts:** the two ISO-8859-1 mono fonts already used: `S = FONT_5X7` (5×7, advance 5),
  `M = FONT_6X10` (6×10, advance 6). See §5.4 for scaling beyond M.

### 5.3 Top-level layout JSON

Compact keys to fit flash. Example:

```json
{ "v": 1, "e": [
  { "t": 3, "x": 1, "y": 0,  "w": 62, "s": 1, "c": 0, "a": 0 },
  { "t": 1, "x": 0, "y": 11, "w": 64, "h": 52, "n": 3, "c": 0, "sc": 1, "sd": 1, "st": 1, "hc": 0 },
  { "t": 4, "x": 44, "y": 0, "s": 1, "c": 1, "a": 2 }
] }
```

- `v` — schema version (`u8`), currently `1`.
- `e` — array of elements, max `MAX_ELEMENTS` (§5.5). An **empty `e`** means "no custom
  layout" (equivalent to reset-to-default).

### 5.4 Element schema (flat struct, numeric `t` tag)

All elements share `t` (type), `x`, `y`. Other fields are type-specific and defaulted.

| `t` | Type | Fields (beyond t,x,y) | Renders as (firmware primitive) |
|---|---|---|---|
| `0` | **Text** (static) | `w` (clip/marquee width), `s` (font 0=S,1=M), `k` (scale 1–3), `c` (colour), `a` (align 0=L/1=C/2=R), `v` (literal string) | `draw_marquee` / `left` / `centered` |
| `1` | **Departures** (live block) | `w`, `h`, `n` (rows 1–4), `rh` (row height, default 17), `sc` (show badge), `sd` (show destination), `st` (show time), `hc` (hide-city) | parameterized `draw_departures` |
| `2` | **Station name** (live) | `w`, `s`, `k`, `c`, `a`, `hc` (hide-city) | `draw_marquee` bound to station |
| `3` | **Clock** (live) | `s`, `k`, `c`, `a`, `f` (format 0=`HH:MM`,1=`H:MM`, …) | `left`/`centered` of formatted time |
| `4` | **Date** (live) | `s`, `k`, `c`, `a`, `f` (format) | as Clock |
| `5` | **Divider** (rule) | `w` (length), `th` (thickness 1–2), `c` | `rule` / `Line` |
| `6` | **Icon** | `k` (scale 1–3), `c`, `g` (glyph id: 0=tram-front,1=Z-blind,2=arrow) | `draw_train_front` / glyph blitter |

Notes:
- `v` (Text literal): bounded `String<N>` on the firmware side (see §5.5). Watch the
  unescape budget in §6.
- **Scaling (`k`) & fonts (v1 requirement):** embedded-graphics mono fonts are fixed-size;
  there is no native scale. v1 supports **both** the two real fonts (`s` = S/M) **and**
  **integer upscaling `k ∈ {1,2,3}`**. Upscaling is implemented with a small **glyph pixel-
  doubling blitter** in the firmware: read the chosen mono font's per-glyph bitmap and draw
  each source pixel as a `k×k` block (so `M` at `k=2` yields a 12×20 glyph). Text metrics used
  for layout/marquee/clip math scale accordingly (advance = `char_w × k`, height = `font_h ×
  k`). The simulator implements the **identical** blitter so WYSIWYG holds glyph-for-glyph.
  Applies to every text-bearing type (Text, Station, Clock, Date).
- **Data binding** is implicit in the type: types `1–4` pull from live runtime data at draw
  time (departures, station name, `now_unix`); types `0,5,6` are static.

### 5.5 Bounds & validation (enforced on **both** phone and firmware)
- **`LAYOUT_MAX_BYTES` — the authoritative flash bound (recommend 1536).** A layout is valid
  only if its serialized JSON is ≤ this. This — not element count — is what guarantees the
  record fits the sector; see §6 point 2 for why (accented-text escapes).
- `MAX_ELEMENTS` — recommend **16**, a secondary sanity limit that bounds the heapless `Vec`.
- Text literal `v` — recommend `String<24>` (bounds a single field + the storage unescape
  buffer, §6). The `LAYOUT_MAX_BYTES` cap still governs the total.
- Numeric ranges clamped: `x,y,w,h ∈ 0..=64`, `n ∈ 1..=4`, `k ∈ 1..=3`, `c ∈ 0..=2`,
  `a ∈ 0..=2`, indices within their enums.
- The firmware must **defensively clamp/skip** any out-of-range value rather than trust the
  payload (a hand-crafted POST must never panic the render task). Elements fully off-panel are
  skipped; partially off-panel are clipped by the existing `pset`/clip helpers.
- **Forward-compat:** unknown `t` values and unknown fields are ignored (`#[serde(default)]`);
  a layout with a newer `v` than the firmware understands is treated as "no custom layout"
  (fall back to built-in) rather than mis-rendered.

---

## 6. Persistence & storage impact (flash — read carefully)

The layout is added to the single persisted record `Persisted { wifi, selection, config,
layout }` in `storage.rs`, as a new **`#[serde(default)] layout: Option<Layout>`** field
(forward-compatible: old records without it load as `None`). `save_layout` / `load_layout`
follow the existing read-modify-write pattern (`save_config` is the template).

**This is the highest-risk part of the feature.** Everything lives in **one 4096-byte flash
sector** as a single JSON record behind an 8-byte header, so the physical ceiling is
`4096 − 8 = 4088 B`. The record's current whole-record worst case (WiFi creds + a full
`MAX_CONNS` selection + config) is **~900 B**, so the sector is only ~22 % used — there is
plenty of *physical* room. The blocker is instead an **artificial software cap**,
`MAX_PAYLOAD = 1024`, well below the physical ceiling; a layout won't fit under *that*.
Required changes:

1. **Bump `MAX_PAYLOAD`** (in `storage.rs`) from `1024` to **~3072**. The whole `Persisted`
   record must serialize under both `MAX_PAYLOAD` **and** the sector's usable 4088 B. With the
   existing record at ~900 B and the layout capped at ~1.5 KB (point 2), the worst case is
   **~2.4 KB** — under 3072, and ~1.6 KB below the 4088 physical ceiling. ✔

2. **Cap the layout by TOTAL SERIALIZED BYTES, not by element count.** Define
   `LAYOUT_MAX_BYTES` (recommend **1536**) and treat it as the authoritative bound; keep
   `MAX_ELEMENTS = 16` only as a secondary sanity limit. Element count alone does **not**
   guarantee a fit: a Text element's string is `String<24>`, but accented Swiss characters
   (`ü`, `ö`, `ä`) serialize as 6-byte `\uXXXX` escapes, so one worst-case Text element is
   ~200 B and 16 of them would be ~3.2 KB — which, on top of the existing ~900 B, would blow
   past 4088. Enforce the byte cap in **two** places:
   - **Editor (live):** the page knows the working layout's serialized size as the user edits,
     so it disables **+ Add** and shows a "layout full" note *before* Save once the next
     element would cross `LAYOUT_MAX_BYTES`. This makes the limit visible, never a silent
     truncation.
   - **Firmware (backstop):** `POST /layout` (and `POST /preview`) reject a body whose
     serialized layout exceeds `LAYOUT_MAX_BYTES` with a clear error, before it can be written
     to flash. Add a test asserting the largest *accepted* layout still serializes under budget.

   In practice, laying out 64×64 leaves no room to place many max-length text boxes, so real
   layouts land near ~1 KB; the byte cap simply guarantees correctness for the pathological
   case rather than relying on that.
3. **Stack:** `write_record` builds `buf: [0u8; 8 + MAX_PAYLOAD + 4]` on the stack (~3 KB at
   the new size) and `read_record` a `[0u8; MAX_PAYLOAD]` scratch. Confirm the network/httpd
   task stack (where `/layout` and `/save` handlers run) has headroom; bump the task stack if
   needed. Consider making these buffers `static`/pooled if stack pressure appears.
4. **Unescape buffer:** `read_record` uses `let mut unescape = [0u8; 96]` sized for "the
   longest field value". An accented char (`ü`) serializes as `ü` (6 B); a `String<24>`
   text field of all-accented chars → 144 B > 96. **Bump the unescape buffer to ≥ 256 B**, or
   cap the Text literal length so its escaped form fits. (Station names already flow through
   here; re-verify against the new max field.)
5. **HTTP buffers (`httpd.rs`):** `POST /layout` and `POST /preview` bodies can reach
   `LAYOUT_MAX_BYTES` (~1.5 KB) plus request headers. Today `http_buf = 2048`, `tcp_rx = 1024`,
   so the body alone can exceed `tcp_rx`. **Bump `http_buf` to ~3072 and `tcp_rx` to ~2048**;
   `tcp_tx = 4096` is fine for the `GET /layout` response. Verify picoserve's buffering handles
   the largest request end-to-end.

All five must be done together; changing the schema without the buffer/stack bumps will
produce silent save failures (the exact "polling doesn't start after restart" class of bug
already noted in `storage.rs`).

---

## 7. Firmware changes

### 7.1 `model.rs`
- Add `Layout { v: u8, e: Vec<Element, MAX_ELEMENTS> }` and a **flat** `Element` struct with a
  numeric `t` tag and `#[serde(default)]` optional fields per §5.4 (heapless `String<24>` for
  text). Add `pub const MAX_ELEMENTS: usize = 16;`. Derive `Clone, Debug, Serialize,
  Deserialize`. Use short serde `rename` keys matching §5.
- Do **not** use a data-carrying Rust enum for elements (serde-json-core limitation, §5.1).

### 7.2 `storage.rs`
- Add `layout: Option<Layout>` to `Persisted` (with `#[serde(default)]`).
- Add `load_layout()` / `save_layout()` / (optional) `clear_layout()` mirroring
  `load_config`/`save_config`. `clear_all` (BOOT reset, PB §7.9) already rewrites the empty
  default, so it clears the layout for free.
- Apply the buffer/size changes from §6.

### 7.3 `shared.rs`
- Add a **live layout mirror** for the render task — the layout the panel currently draws.
  Because a `Layout` is larger than an atomic, store it behind a `Mutex<CriticalSectionRawMutex,
  Option<Layout>>` (like `SELECTION`), **not** the render-task-must-never-block atomics used for
  config scalars. The render task reads it when drawing the Departures state; acceptable because
  a departures redraw is already event-driven, not per-DMA-frame.
- `apply_layout(Option<Layout>)` sets the live mirror and signals a redraw. Called at boot
  (from flash), on `POST /layout` (persisted), and on `POST /preview` (transient).
- **Preview state** for the live on-panel preview (§4.3): a flag that the mirror currently
  holds a *transient* (unsaved) layout, plus a preview **deadline** (`AtomicI64`/`AtomicU32`
  holding an `Instant`-derived expiry). Set on each preview push; cleared when preview ends or
  is committed. A lightweight watchdog (see §7.4) reverts the mirror to the persisted layout
  when the deadline passes.

### 7.4 `httpd.rs` — new endpoints
- `GET /layout` → current **persisted** layout JSON (or `{"v":1,"e":[]}` / `204` when none),
  read from flash / the persisted copy. Used by the editor to seed its working copy and by the
  main-page thumbnail. Use an `OwnedJson<N>`-style response (N sized to the layout budget).
- `POST /layout` (`Json<Layout>`) → validate/clamp (§5.5) and **reject if the serialized layout
  exceeds `LAYOUT_MAX_BYTES`** (§6 pt. 2) before any flash write; on success persist via
  `save_layout`, update the live mirror via `apply_layout`, clear any preview state, and
  `SELECTION_CHANGED.signal(())` to force an immediate redraw (same wake used by `/save` and
  `/config`). Respond `{"ok":true}` (or an error on over-budget/invalid input).
- `POST /preview` (`Json<Layout>`) → **transient** live preview (§4.3). Validate/clamp and apply
  the same `LAYOUT_MAX_BYTES` check, push to the live mirror via `apply_layout` **without**
  touching flash, mark preview active, and (re)arm
  the auto-revert deadline (~15 s). Signals a redraw. Respond `{"ok":true}`. This is the
  high-frequency endpoint (debounced edits + ~5 s keepalive), so it must not write flash.
- `POST /preview/end` → discard the transient preview: reload the **persisted** layout and
  `apply_layout` it, clear preview state. Called on editor Cancel (and harmlessly after Save).
- **Auto-revert watchdog:** while preview is active, a timer (a small dedicated task, or folded
  into the existing render/poll timing) checks the deadline; on expiry it behaves exactly like
  `POST /preview/end` so an abandoned session (phone locked / WiFi dropped / tab closed) cannot
  leave the panel stuck on an unsaved draft.
- **Reset:** a `POST /layout` with empty `e` clears the saved layout (kept as one route rather
  than a separate `DELETE`, to keep the table minimal).
- Register all routes in `config_server_task`'s `Router`. The `/preview` body is the same size
  as `/layout`, so the §6 HTTP-buffer sizing already covers it.

### 7.5 `display.rs` — the renderer (the core work)
- The Departures branch of `draw_state` gains a fork:
  ```
  DisplayState::Departures { station, deps } =>
      match live custom layout {
          Some(layout) => draw_custom_layout(fb, layout, station, deps, frame),
          None          => draw_departures(fb, station, deps, frame),   // unchanged built-in
      }
  ```
- `draw_custom_layout` iterates `layout.e` in order and dispatches on `t`, reusing existing
  primitives: `left` / `centered` / `draw_marquee` / `draw_marquee_clipped` (text, station,
  clock, date), a parameterized extraction of `draw_departures`' row logic (Departures block:
  badge via `draw_badge`, destination marquee, time / `draw_train_front`), `rule` (divider),
  and `draw_train_front` / the Z-blind / `arrow` glyphs (icon). Colours resolve through the
  palette map and the existing `scaled()` brightness choke point.
- **Font scaling (`k`, v1):** implement a `blit_scaled_text` helper that reads a mono font
  glyph bitmap and draws each source pixel as a `k×k` block, with matching scaled metrics for
  the marquee/clip helpers. All text-bearing types route through it. All drawing stays behind
  the "one isolated function" rule from PB §7.7.
- **Animation:** an element mid-marquee makes the frame "animating"; OR the per-element
  scrolling flags and return `true` so the render loop keeps ticking (same contract as
  `draw_departures`). The clock/date do not themselves force animation; they refresh on the
  existing `BRIGHTNESS_REFRESH_SECS` static-screen wake, which is adequate for `HH:MM`.
- **Defensive rendering:** clamp/skip out-of-range or off-panel elements; never panic
  (`pset` already clips). An empty layout (`e == []`) is treated as "no custom layout" →
  built-in board.
- **Other states unchanged:** Provisioning / Connecting / IdleAddress / Offline keep their
  built-in rendering (§3 non-goals).

### 7.6 Boot
- Load the layout from flash at boot alongside `wifi`/`selection`/`config` and push it into
  the live mirror via `apply_layout` before the first departures render.

---

## 8. Web / config-page changes (`web/index.html`)

Everything stays inline (no new files, no CDNs — PB §4.1 / §8-7), consistent with the current
self-contained page.

### 8.1 Entry card + thumbnail
- Add the "Design your board" secondary card beneath `#save-section` (§4.1). The thumbnail is
  a small `<canvas>` rendered by the same simulator draw routine at reduced scale, refreshed
  after each successful save and on page load (from `GET /layout`).

### 8.2 The simulator (fidelity is the whole point)
- A `<canvas>` renderer that draws the 64×64 grid as LED dots and paints elements **using the
  same fonts, palette, coordinate system, and layout math as the firmware**. To be truly
  WYSIWYG:
  - **Fonts:** port the two ISO-8859-1 mono fonts (5×7, 6×10) into the page as compact glyph
    bitmaps (a small base64 atlas or a JS byte table) and blit them per-pixel, with the same
    `k×k` integer upscaling as the firmware. This guarantees the preview matches the panel
    glyph-for-glyph — essential now that the physical panel mirrors the design live (§4.3), so
    any mismatch between simulator and panel would be visible side by side.
  - **Palette:** reuse the exact copper/amber/dim RGB values from `display.rs` (`ACCENT`,
    `AMBER`, `DIM`) rather than approximations.
  - **Marquee/clip/badge math:** mirror `draw_marquee`, `draw_marquee_clipped`, `draw_badge`
    and the departures-row layout so wrapping, clipping and badge sizing look identical.
- The simulator is the single source of truth for both the editor canvas and the thumbnail;
  factor it as one draw function taking `(layout, data, scale, frameOrStatic)`.

### 8.3 Editor overlay & interactions
- Build the overlay, app bar, canvas, palette, selection chrome, and properties sheet per §4.
- Hit-testing: map touch coordinates → LED coordinates via the current scale; select the
  top-most element whose bounds contain the point; handle drags on body (move) and handles
  (resize) with 1-LED snapping and clamping.
- Enforce all §5.5 bounds client-side (belt-and-suspenders with the firmware).

### 8.4 Networking & live-preview driver
- `GET /layout` on page load (thumbnail) and on editor open (seed the working copy).
- **Live on-panel preview (§4.3):** on editor open, immediately `POST /preview` with the
  working copy; on every edit, `POST /preview` **debounced** ~150–250 ms; while idle in the
  editor, a **keepalive** `POST /preview` every ~5 s to hold the panel in preview and reset the
  firmware auto-revert timer. Preview posts are fire-and-forget (a dropped one is corrected by
  the next edit or keepalive) and must not block the UI — coalesce so only the latest working
  copy is in flight.
- **Save:** `POST /layout` (persist), then `POST /preview/end`; update the thumbnail and close.
- **Cancel:** `POST /preview/end` (panel reverts to the saved layout), then close.
- **Reset:** `POST /layout` with empty `e`.
- Reuse the optimistic status pattern and 8 s abort timeout already used by `/save` for the
  Save / Reset calls; keepalive/preview calls use a short timeout and no user-facing error.

---

## 9. Edge cases & constraints

- **Malformed / hostile POST:** firmware clamps and skips; never panics (§5.5, §7.5).
- **Layout references live data that's absent:** Departures/Station render "no service"/empty
  gracefully (as the built-in board already does); Clock/Date before SNTP sync render a
  placeholder (`--:--`) rather than a wrong time (`now_unix()` returns `None` pre-sync).
- **Empty layout saved:** treated as reset-to-default (built-in board).
- **Over-budget layout:** the editor prevents it (disables Add at the byte cap, §4.4); if one
  still arrives — a crafted POST — the firmware rejects it with an error and does **not** write
  flash (§6 pt. 2 / §7.4), so a too-large layout can never partially overwrite the record.
- **Newer schema `v` than firmware:** fall back to built-in (§5.5).
- **BOOT-button reset (PB §7.9):** clears the layout along with everything else.
- **Flash write fails:** same failure surface as `/save` today — the layout applies live this
  session but a log error notes it won't survive reboot; the page shows the error.
- **Overlapping elements:** allowed; draw order = array order; last wins. Optional z-controls.
- **Phone scroll vs. drag:** `touch-action:none` on the canvas; page scroll disabled while a
  drag is in progress.
- **Editor abandoned mid-preview** (phone locks, WiFi drops, tab closed without Cancel): the
  firmware auto-revert watchdog (§7.4) restores the saved layout after ~15 s, so the panel
  never stays stuck on an unsaved draft.
- **Preview races the poll task:** a `POST /preview` only swaps the layout mirror; the live
  departures data still comes from the normal poll pipeline, so the panel shows the working
  layout with real data and never fabricates departures.

## 10. Build sequence

A suggested order that keeps each step independently verifiable. All of it is v1.

1. **Schema + storage plumbing.** `Layout`/`Element` types, `Persisted.layout`, the §6 buffer
   and stack bumps, `GET`/`POST /layout`, `apply_layout`, boot load. *Verify:* a hand-POSTed
   layout persists and reloads across reboot (via logs).
2. **Firmware renderer + scaling.** `draw_custom_layout` for all element types (§5.4), plus the
   `blit_scaled_text` helper so both fonts (`s`) and integer scale (`k ∈ {1,2,3}`) work. *Verify:*
   a POSTed layout draws on the panel at any font/scale; empty layout falls back to the built-in
   board.
3. **Live-preview endpoints + watchdog.** `POST /preview`, `POST /preview/end`, the transient
   mirror, and the auto-revert timer (§7.4). *Verify:* a POSTed preview shows on the panel without
   writing flash; the panel reverts on `/preview/end` and after the timeout.
4. **Simulator + thumbnail.** Pixel-accurate JS renderer (fonts, palette, scaling blitter, marquee
   math) driving both the editor canvas and the main-page thumbnail (`GET /layout`). *Verify:* the
   simulator matches the panel glyph-for-glyph for every element type and scale.
5. **Editor.** Overlay, palette/add, move/resize/delete, properties sheet with colour/font/scale/
   align controls and nudges, dirty tracking, Save/Cancel/Reset — wired to the live-preview driver
   (§8.4) so the panel mirrors from open to close. *Verify:* full design → live panel mirroring →
   save → persisted, and Cancel/abandon reverts.
