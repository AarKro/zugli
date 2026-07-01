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
Exactly **one** custom layout is stored at a time (no versioning, no history). The editor
loads the saved custom layout if present, otherwise starts from an empty canvas.

The device's departures screen is drawn according to a user-selected **UI mode**, chosen from
a three-way selector on the config page: **Default** (the built-in departures board, PB §7.7),
**Focus** (the existing focus view), or **Custom** (the user's built layout). The custom
layout is shown **only when Custom is selected** — it never overrides Default or Focus merely
by existing. This reworks the current two-way Default/Focus toggle into a three-way selector;
the Focus view itself is unchanged.

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
- **Reversible by design.** The custom layout only renders when the user picks **Custom**;
  one tap on **Default** (or **Focus**) in the mode selector restores a built-in view
  instantly, so the feature is safe to ship — worst case, the user switches modes.

### Target user & job-to-be-done
"I've mounted Zügli on my shelf. I want the stop name bigger, a clock in the corner, and only
two departure rows — laid out the way *I* want, from my phone, in under two minutes, without
flashing new firmware."

---

## 3. Goals and non-goals

### Goals
- A **three-way UI-mode selector** (Default / Focus / Custom) on the config page that chooses
  what the panel draws. The custom layout renders only when Custom is selected.
- A full-screen, touch-first layout editor reachable from the config page.
- A **pixel-accurate** 64×64 simulator that mirrors the firmware's fonts, palette, and
  coordinate system.
- **Live on-panel preview:** the physical panel mirrors the working layout in real time from
  the moment the editor opens, so the phone simulator and the device stay in lock-step while
  the user designs.
- Add / move / resize / delete elements; edit per-element properties; nudge for pixel
  precision; clear the custom layout.
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

### 4.1 Main-page controls: UI-mode selector + builder entry

Two related controls sit together on the main config page, directly beneath the
`save-section`.

**(a) UI-mode selector (segmented control).** A full-width, three-segment control —
**Default | Focus | Custom** — reworking the existing two-way Default/Focus toggle. It uses
the same visual language as the tracking-mode switch already at the top of the page (`.mode`
segments: the active segment highlighted like a selected connection). Selecting a segment:
- writes the new mode to the board immediately (optimistic `POST /config`, §7.4/§8.4 — the
  same pattern the settings sheet uses), so the panel switches live;
- **Default / Focus** switch the panel to the respective built-in view at once;
- **Custom** shows the user's saved layout. If **no** custom layout exists yet, tapping Custom
  instead **opens the builder** (create flow, §4.2) rather than switching the panel to an
  empty screen; Custom becomes the live mode once a layout is saved (§4.3 / §4.6).

**(b) Builder entry card.** A full-width **secondary card/button** labelled **"Design your
board →"** with a small **live thumbnail** of the current custom layout (a scaled-down,
non-interactive render of the simulator) on its left. The caption reads "No custom layout yet"
when none exists, otherwise "Custom layout". Tapping it opens the full-screen editor (§4.2).

Rationale:
- **Discoverability & industry standard** (cf. LaMetric, Divoom Pixoo, smart-display "design"
  tabs): the selector makes the three views a first-class, one-tap choice, and the thumbnail
  communicates the custom design's state *and* invites editing in one glance.
- **Separation of concerns.** The gear/settings sheet stays about *display toggles* (brightness,
  hide-city, badges); the main-page controls own *which view* and *composition*. This keeps
  the primary flow (pick a stop → save) uncluttered.
- **Selecting ≠ designing.** Choosing the Custom *mode* and *editing* the custom layout are
  distinct actions with distinct controls, so a user can switch to Custom without being forced
  into the editor once a layout exists.

The card and selector are a distinct visual weight from the primary `Save to Zügli` button
(secondary styling: surface background, copper text/outline) so they never compete with the
primary save action.

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
- **Transient, never persisted, mode-independent.** `/preview` updates the device's *live*
  layout mirror only; nothing is written to flash until **Save** (§4.6). The preview shows the
  working layout **regardless of the current UI mode** — you can design a Custom layout while
  the persisted mode is still Default or Focus, and the panel previews your draft the whole
  time. A reboot or a timeout (below) reverts to the persisted mode + layout.
- **Idle keepalive + auto-revert safety.** While the editor is open the page sends a keepalive
  `POST /preview` (reusing the latest working copy) every ~5 s. The firmware arms an
  **auto-revert timer** (~15 s) on each preview push; if it expires without a new push — the
  phone locked, lost WiFi, or the tab was closed — the device leaves preview mode and reverts
  to its persisted UI mode + layout, so the panel can never get stuck showing an abandoned draft.
- **On close (Save or Cancel):** the editor ends preview explicitly (`POST /preview/end`, §7.4).
  A **Save** persists the layout **and sets the UI mode to Custom** (§4.6), so the design stays
  on the panel; a **Cancel** reverts the device to its persisted mode + layout (whatever it was
  before editing).

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
  the element's type (§5.4). Common controls: **colour** (see below), **alignment**
  (left/center/right), **font size** (S/M/L), plus the x/y/w/h + nudges. Type-specific
  controls: Text → text input; Departures → row count / row height (its badges and
  city-stripping follow the global **Line badges** / **Hide city names** settings — no
  per-element toggles); Station → nothing extra (city-stripping is global); Clock/Date →
  format; Icon → which glyph.
- **Colour control (presets + custom picker):** a row of tappable swatches — the three brand
  presets (**amber / copper / dim**, the good defaults) followed by a **Custom** swatch. The
  Custom swatch is a thin wrapper over a **native `<input type="color">`** (the OS colour
  wheel/eyedropper — exactly what "native picker" means on a phone); tapping it opens the OS
  picker. Behaviour:
  - Picking a **preset** sets the element's `c` and **clears** any `col`.
  - Picking a **custom** colour sets `col` (`0xRRGGBB` from the `<input>`'s hex value) which
    overrides `c`; the Custom swatch then shows that colour and reads as selected.
  - The active swatch reflects the element's current colour on open (a preset highlighted, or
    the Custom swatch showing `col`).
  - Every colour change flows through the same live preview + dirty tracking as any other edit,
    so the panel (§4.3) and the simulator update as the user drags the picker.
  - The Departures block (`t=1`) shows only presets — it keeps the board's two-tone scheme
    (§5.4), so no Custom swatch there.
- **Delete:** a **trash** button in the properties sheet (with an inline confirm on a phone —
  a second tap or a small confirm chip). Deselecting (tap empty canvas) returns to the
  palette.
- **Deselect:** tap empty canvas area.

### 4.6 Save / cancel / dirty tracking

- The editor keeps a working copy of the layout array. Any mutation marks it **dirty** and
  enables **Save**.
- **Save:** `POST /layout` with the serialized layout (§5), then set the UI mode to **Custom**
  (`POST /config`, §7.4) so the freshly-saved design is what the panel shows. Optimistic UX
  identical to the existing `/config` flow (`applyChange` in `web/index.html`): show "Saved —
  Zügli is updating." on success; on failure keep the editor open, surface "Couldn't reach
  Zügli — try again.", and do not close. On success, update the main-page thumbnail, reflect
  **Custom** as the selected mode in the selector (§4.1), and close the editor.
- **Cancel:** if dirty, confirm ("Discard changes?"); otherwise close immediately.
- **Optional single-step in-editor undo:** a lightweight `Ctrl-Z`-style "Undo" affordance that
  reverts the last edit *within the current editing session only*. This is not persisted
  history and does not contradict the "one layout, no history" storage rule. Ship only if
  cheap; otherwise omit.

### 4.7 Clear custom layout

- A **"Clear custom layout"** action lives in the editor (e.g. an overflow item in the app bar,
  or a footer button under the palette). It clears the saved layout — `POST /layout` with an
  **empty layout** (§7.4) — and, because there is then nothing custom to show, sets the UI mode
  back to **Default** (`POST /config`), after a confirm dialog. The main-page thumbnail reverts
  to "No custom layout yet" and the selector returns to **Default**. (This does not touch the
  Focus view.)

### 4.8 Relationship to existing display settings (important)

The settings sheet (PB §4.6) has **Hide city names** and **Line badges** toggles that shape
the built-in board. These are **global** and apply in **every** UI mode — Default, Focus, and
Custom alike. The custom layout's data-bound elements (Departures, Station) deliberately carry
**no** per-element overrides for them (§5.4); they read the same global config through the same
`city()` / `line_badges_enabled()` paths as the built-in board.

- **Hide city names / Line badges:** one setting, one behaviour, everywhere. Toggling them in
  the settings sheet immediately reshapes the Default board, the Focus view, **and** any
  Departures/Station elements in a custom layout. No special-casing, no "applies only to…"
  caveat, no confusing dual controls.
- **Brightness / auto-dim** are likewise global and **always apply** (they scale the whole
  palette at the render choke point — PB §7.7 / `display::scaled`), in every mode.

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
- **Colour** — a preset **or** a custom RGB:
  - **Preset** `c` (indices map to existing `display.rs` constants, scaled by brightness at
    draw): `0 = AMBER`, `1 = ACCENT` (copper), `2 = DIM`. (`OFF` is only for badge cut-outs,
    not a user colour.) `c` defaults to `0`.
  - **Custom** `col` — an optional 24-bit RGB value `0xRRGGBB` (see §5.4). When present it
    **overrides** `c`; when absent the preset `c` is used. Chosen via the native colour picker
    (§4.5). Still passes through the brightness `scaled()` choke point at draw, so it dims with
    the rest of the panel. Note the HUB75 gamut caveat in §9 (bright/high-blue customs wash
    toward white on the physical panel).
- **Fonts:** the two ISO-8859-1 mono fonts already used: `S = FONT_5X7` (5×7, advance 5),
  `M = FONT_6X10` (6×10, advance 6). See §5.4 for scaling beyond M.

### 5.3 Top-level layout JSON

Compact keys to fit flash. Example:

```json
{ "v": 1, "e": [
  { "t": 3, "x": 1, "y": 0,  "w": 62, "s": 1, "c": 0, "a": 0 },
  { "t": 1, "x": 0, "y": 11, "w": 64, "h": 52, "n": 3 },
  { "t": 4, "x": 44, "y": 0, "s": 1, "col": 5296264, "a": 2 }
] }
```

(`col` is the optional custom colour: `5296264 = 0x50D888`, a green not in the preset palette;
omit it to fall back to the preset `c`.)

- `v` — schema version (`u8`), currently `1`.
- `e` — array of elements, max `MAX_ELEMENTS` (§5.5). An **empty `e`** means "no custom
  layout saved"; in Custom mode the firmware falls back to the Default board (§7.5).

### 5.4 Element schema (flat struct, numeric `t` tag)

All elements share `t` (type), `x`, `y`. Other fields are type-specific and defaulted.

| `t` | Type | Fields (beyond t,x,y) | Renders as (firmware primitive) |
|---|---|---|---|
| `0` | **Text** (static) | `w` (clip/marquee width), `s` (font 0=S,1=M), `k` (scale 1–3), `c` (colour), `a` (align 0=L/1=C/2=R), `v` (literal string) | `draw_marquee` / `left` / `centered` |
| `1` | **Departures** (live block) | `w`, `h`, `n` (rows 1–4), `rh` (row height, default 17) — **no content toggles**; badges + city-stripping follow global config | parameterized `draw_departures` |
| `2` | **Station name** (live) | `w`, `s`, `k`, `c`, `a` — city-stripping follows global config | `draw_marquee` bound to station |
| `3` | **Clock** (live) | `s`, `k`, `c`, `a`, `f` (format 0=`HH:MM`,1=`H:MM`, …) | `left`/`centered` of formatted time |
| `4` | **Date** (live) | `s`, `k`, `c`, `a`, `f` (format) | as Clock |
| `5` | **Divider** (rule) | `w` (length), `th` (thickness 1–2), `c` | `rule` / `Line` |
| `6` | **Icon** | `k` (scale 1–3), `c`, `g` (glyph id: 0=tram-front,1=Z-blind,2=arrow) | `draw_train_front` / glyph blitter |

Notes:
- **Config-driven behaviour — no per-element overrides.** The data-bound elements render
  exactly like the built-in board: the **Departures** block and **Station name** honour the
  **global config** (`stripCity` "Hide city names", `showLineBadges` "Line badges") at draw
  time via the existing `city()` / `line_badges_enabled()` paths. There are deliberately **no**
  per-element toggles that duplicate a config option (no `hc` / `sc` / `sd` / `st`); placing an
  element only decides *where/how big* it is, not a divergent content behaviour. This keeps a
  single source of truth for those settings and matches how Default/Focus already behave.
- **Colour (`c` + optional `col`):** every element that has a `c` slot may instead carry an
  optional **`col`** (24-bit RGB `0xRRGGBB`, stored as a `u32`) that overrides the preset `c`.
  Absent `col` → use the preset. This applies to the single-colour elements (Text, Station,
  Clock, Date, Divider, Icon). The **Departures block keeps its two-tone board scheme**
  (amber text / copper time / badge, exactly as the built-in board) and takes **neither `c`
  nor `col`** — recolouring the board is out of scope.
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
- `col` (optional custom colour) — a `u32` masked to 24 bits (`col & 0xFFFFFF`); any value is
  otherwise valid. When present it overrides `c`. On the Departures block (`t=1`) `col` is
  ignored (§5.4).
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
- **Colour field.** Element carries `c: u8` (preset index, `#[serde(default)]` → 0) and an
  optional custom colour `col: Option<u32>` (`0xRRGGBB`). Tag `col` with
  `#[serde(default, skip_serializing_if = "Option::is_none")]` so it only appears in the JSON
  when a custom colour is set — keeping the common preset case compact for flash. Add an
  `elem_color` helper (used by the renderer, §7.5) that returns the `col` RGB when present,
  else the preset for `c`.
- **UI mode.** Replace the existing boolean Default/Focus toggle in `Config` (the uncommitted
  `focus`-style field) with a three-state **`ui_mode`** (JSON `uiMode`): `0 = Default`,
  `1 = Focus`, `2 = Custom`. Represent it as a `u8` (or a small `#[repr(u8)]` enum that
  serializes as an integer) with `#[serde(default)]` → `0`, so older flash records and the
  existing config round-trip. This lives in `Config`, so it persists and is edited through the
  existing `/config` endpoint (§7.4). The old boolean maps forward as `false → Default`,
  `true → Focus`.

### 7.2 `storage.rs`
- Add `layout: Option<Layout>` to `Persisted` (with `#[serde(default)]`).
- Add `load_layout()` / `save_layout()` / (optional) `clear_layout()` mirroring
  `load_config`/`save_config`. `clear_all` (BOOT reset, PB §7.9) already rewrites the empty
  default, so it clears the layout for free.
- Apply the buffer/size changes from §6.

### 7.3 `shared.rs`
- Add a **live layout mirror** for the render task — the persisted custom layout. Because a
  `Layout` is larger than an atomic, store it behind a `Mutex<CriticalSectionRawMutex,
  Option<Layout>>` (like `SELECTION`), **not** the render-task-must-never-block atomics used for
  config scalars. The render task reads it (in Custom mode) when drawing the Departures state;
  acceptable because a departures redraw is already event-driven, not per-DMA-frame.
- **UI mode accessor.** Add `ui_mode()` reading the `uiMode` field mirrored from `Config`
  (fold it into the existing config live-mirror / `apply_config`, alongside brightness etc.),
  so the render dispatch (§7.5) is a cheap read.
- `apply_layout(Option<Layout>)` sets the persisted-layout mirror. Called at boot (from flash)
  and on `POST /layout`.
- **Preview accessors** for §7.5: a `preview_active()` flag and the transient `preview_layout()`
  set by `POST /preview` and cleared by `POST /preview/end` / the watchdog. Keep the transient
  preview layout separate from the persisted-layout mirror so ending preview restores the real
  persisted mode + layout without a re-fetch.
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
- `POST /preview/end` → discard the transient preview: leave preview mode and re-render the
  device's **persisted UI mode + layout** (Default / Focus / Custom), clear preview state.
  Called on editor Cancel (and harmlessly after Save). Because preview is mode-independent
  (§4.3), this is how the panel returns to whatever mode was selected before editing.
- **Auto-revert watchdog:** while preview is active, a timer (a small dedicated task, or folded
  into the existing render/poll timing) checks the deadline; on expiry it behaves exactly like
  `POST /preview/end` so an abandoned session (phone locked / WiFi dropped / tab closed) cannot
  leave the panel stuck on an unsaved draft.
- **UI mode is *not* a new endpoint.** Selecting Default / Focus / Custom is a `Config` change,
  so it flows through the **existing `GET`/`POST /config`** (`uiMode` field, §7.1). The
  main-page selector, the Save-sets-Custom step (§4.6), and Clear-sets-Default (§4.7) all POST
  `/config`. The clamp on `POST /config` must reject an out-of-range `uiMode`.
- **Clear custom layout:** a `POST /layout` with empty `e` clears the saved layout (kept as one
  route rather than a separate `DELETE`, to keep the table minimal); the page pairs it with a
  `POST /config` setting `uiMode = Default`.
- Register all routes in `config_server_task`'s `Router`. The `/preview` body is the same size
  as `/layout`, so the §6 HTTP-buffer sizing already covers it.

### 7.5 `display.rs` — the renderer (the core work)
- The Departures branch of `draw_state` dispatches on the **UI mode** (`shared::ui_mode()`),
  **not** on whether a custom layout exists — a custom layout never renders unless Custom is
  the selected mode. During a live preview (§4.3) the transient mirror forces the custom path
  regardless of mode:
  ```
  DisplayState::Departures { station, deps } => {
      if preview_active() {
          draw_custom_layout(fb, preview_layout(), station, deps, frame)
      } else {
          match ui_mode() {
              UiMode::Default => draw_departures(fb, station, deps, frame), // unchanged built-in
              UiMode::Focus   => draw_focus(fb, station, deps, frame),      // existing focus view
              UiMode::Custom  => match custom_layout() {
                  Some(l) if !l.e.is_empty() => draw_custom_layout(fb, &l, station, deps, frame),
                  _ => draw_departures(fb, station, deps, frame),           // Custom w/ no layout → Default
              },
          }
      }
  }
  ```
  (`draw_focus` is assumed to already exist; this feature does not modify it.)
- `draw_custom_layout` iterates `layout.e` in order and dispatches on `t`, reusing existing
  primitives: `left` / `centered` / `draw_marquee` / `draw_marquee_clipped` (text, station,
  clock, date), a parameterized extraction of `draw_departures`' row logic (Departures block:
  badge via `draw_badge`, destination marquee, time / `draw_train_front`), `rule` (divider),
  and `draw_train_front` / the Z-blind / `arrow` glyphs (icon).
- **Config-driven, not element-driven.** The Departures and Station paths read the **same
  global config** the built-in board uses — `shared::line_badges_enabled()` for badges vs.
  plain text and `city()` (`shared::strip_city_enabled()`) for city-stripping — **not** any
  per-element flag (there are none, §5.4). The parameterized `draw_departures` takes only
  geometry (origin, width, rows, row height) from the element; all content behaviour comes from
  config, so it stays in lock-step with the Default board and Focus view.
- **Colour resolution:** a single `elem_color(el)` helper returns `Color::new(r,g,b)` from
  `el.col` (unpacking `0xRRGGBB`) when it is present, else the preset `Color` for `el.c`. Its
  result goes through the existing `scaled()` brightness choke point like every other colour,
  so custom colours dim with the panel. The Departures block ignores `col` and keeps its
  amber/copper scheme (§5.4).
- **Font scaling (`k`, v1):** implement a `blit_scaled_text` helper that reads a mono font
  glyph bitmap and draws each source pixel as a `k×k` block, with matching scaled metrics for
  the marquee/clip helpers. All text-bearing types route through it. All drawing stays behind
  the "one isolated function" rule from PB §7.7.
- **Animation:** an element mid-marquee makes the frame "animating"; OR the per-element
  scrolling flags and return `true` so the render loop keeps ticking (same contract as
  `draw_departures`). The clock/date do not themselves force animation; they refresh on the
  existing `BRIGHTNESS_REFRESH_SECS` static-screen wake, which is adequate for `HH:MM`.
- **Defensive rendering:** clamp/skip out-of-range or off-panel elements; never panic
  (`pset` already clips). In Custom mode an empty/missing layout (`e == []`) falls back to the
  Default board, so the panel is never blank.
- **Other states unchanged:** Provisioning / Connecting / IdleAddress / Offline keep their
  built-in rendering (§3 non-goals).

### 7.6 Boot
- Load the layout from flash at boot alongside `wifi`/`selection`/`config` and push it into
  the live mirror via `apply_layout` before the first departures render.

---

## 8. Web / config-page changes (`web/index.html`)

Everything stays inline (no new files, no CDNs — PB §4.1 / §8-7), consistent with the current
self-contained page.

### 8.1 UI-mode selector + entry card
- **Mode selector.** Add a full-width three-segment control (Default / Focus / Custom) beneath
  `#save-section` (§4.1), styled like the existing `.mode` tracking switch. Its value binds to
  `cfg.uiMode`; on tap it calls the existing `applyChange(() => { cfg.uiMode = … })` path
  (optimistic `POST /config`, revert-on-failure). Tapping **Custom** when no saved layout
  exists opens the editor instead of setting the mode (§4.1). Reflect the live `uiMode` after
  Save/Clear and on page load (from `GET /config`).
- **Entry card + thumbnail.** Add the "Design your board" secondary card beneath the selector
  (§4.1). The thumbnail is a small `<canvas>` rendered by the same simulator draw routine at
  reduced scale, refreshed after each successful save and on page load (from `GET /layout`);
  it reads "No custom layout yet" when `e` is empty.

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
    `AMBER`, `DIM`) rather than approximations, and mirror the firmware's `elem_color`
    resolution — draw `col` (`0xRRGGBB`) when present, else the preset `c`.
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
- `GET /layout` on page load (thumbnail) and on editor open (seed the working copy);
  `GET /config` already loads `uiMode` for the selector.
- **UI mode:** the selector POSTs `/config` with the new `uiMode` (existing optimistic path).
- **Live on-panel preview (§4.3):** on editor open, immediately `POST /preview` with the
  working copy; on every edit, `POST /preview` **debounced** ~150–250 ms; while idle in the
  editor, a **keepalive** `POST /preview` every ~5 s to hold the panel in preview and reset the
  firmware auto-revert timer. Preview posts are fire-and-forget (a dropped one is corrected by
  the next edit or keepalive) and must not block the UI — coalesce so only the latest working
  copy is in flight.
- **Save:** `POST /layout` (persist), then `POST /config` setting `uiMode = Custom`, then
  `POST /preview/end`; update the thumbnail, reflect Custom in the selector, and close.
- **Cancel:** `POST /preview/end` (panel reverts to the persisted mode + layout), then close.
- **Clear custom layout:** `POST /layout` with empty `e`, then `POST /config` setting
  `uiMode = Default`.
- Reuse the optimistic status pattern and 8 s abort timeout already used by `/save` for the
  Save / Clear calls; keepalive/preview calls use a short timeout and no user-facing error.

---

## 9. Edge cases & constraints

- **Malformed / hostile POST:** firmware clamps and skips; never panics (§5.5, §7.5).
- **Custom colour vs. HUB75 gamut:** a custom `col` shows accurately on the phone (full-RGB
  screen) but the physical panel skews — the driver comment in `display.rs` notes that colours
  riding high on blue/green wash toward white, which is why the brand palette is deep and
  red-weighted. So a bright or blue-heavy custom colour can look paler/whiter on glass than in
  the simulator. This is inherent to the panel, not a bug; surface a brief note near the
  custom picker ("Colours may look brighter/washed on the panel") and let the live on-panel
  preview (§4.3) be the source of truth. No gamut remapping in v1.
- **Layout references live data that's absent:** Departures/Station render "no service"/empty
  gracefully (as the built-in board already does); Clock/Date before SNTP sync render a
  placeholder (`--:--`) rather than a wrong time (`now_unix()` returns `None` pre-sync).
- **Custom mode with no/empty layout:** the firmware falls back to the Default board so the
  panel is never blank (§7.5); the main-page UI steers around this by opening the editor when
  Custom is tapped without a saved layout, and Clear-custom-layout resets the mode to Default.
- **Mode is Custom, then layout cleared elsewhere** (e.g. BOOT reset wipes the record): `uiMode`
  is also reset to Default by that same wipe; even if it weren't, the §7.5 fallback covers it.
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

1. **UI-mode rework + storage plumbing.** Replace the Default/Focus boolean with `uiMode`
   (`0/1/2`) in `Config` (§7.1); add `Layout`/`Element` types, `Persisted.layout`, the §6 buffer
   and stack bumps, `GET`/`POST /layout`, `apply_layout`, boot load. Dispatch the Departures
   render on `uiMode` (§7.5) with Custom→Default fallback. *Verify:* `uiMode` round-trips via
   `/config`; a hand-POSTed layout persists and reloads across reboot; the panel switches
   Default/Focus/Custom by mode (via logs / a manual `/config` POST).
2. **Firmware renderer + scaling.** `draw_custom_layout` for all element types (§5.4), plus the
   `blit_scaled_text` helper so both fonts (`s`) and integer scale (`k ∈ {1,2,3}`) work. *Verify:*
   in Custom mode a POSTed layout draws on the panel at any font/scale; empty layout falls back
   to the Default board.
3. **Live-preview endpoints + watchdog.** `POST /preview`, `POST /preview/end`, the transient
   mirror, and the auto-revert timer (§7.4). *Verify:* a POSTed preview shows on the panel
   regardless of `uiMode` without writing flash; the panel reverts to the persisted mode + layout
   on `/preview/end` and after the timeout.
4. **Simulator + thumbnail.** Pixel-accurate JS renderer (fonts, palette, scaling blitter, marquee
   math) driving both the editor canvas and the main-page thumbnail (`GET /layout`). *Verify:* the
   simulator matches the panel glyph-for-glyph for every element type and scale.
5. **Main-page selector + editor.** The three-way mode selector (§8.1) wired to `/config`; the
   editor overlay, palette/add, move/resize/delete, properties sheet with colour/font/scale/align
   controls and nudges, dirty tracking, Save (sets Custom) / Cancel / Clear (sets Default) — wired
   to the live-preview driver (§8.4). *Verify:* switch modes from the main page; full design →
   live panel mirroring → save → persisted as Custom; Cancel/abandon reverts; Clear returns to
   Default.
