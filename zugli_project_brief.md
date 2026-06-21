# Zügli — Project Brief & Implementation Plan

> A DIY, ESP32-driven transit departure board for Swiss public transport. A user
> configures **one stop + one connection** from their phone; the device shows the
> live countdown to the next departure of that connection on an LED matrix panel.

**Status:** implementation-ready brief for a coding agent.
**Audience:** the agent (or developer) implementing the web UI and the device firmware.

---

## 0. Name: **Zügli** (settled)

The product is **Zügli**. It's a play on the original **Tramli** (the Swiss-German *-li*
diminutive ending), carried over to *Zug* (train): **Zügli** reads as "little train" in
Swiss German, so the name carries real meaning, not just the vibe. (Earlier working names
— *Tramly*, *NeoTramly*, and the Anglicised *Zügly* — are superseded.)

User-facing usage:

- Setup hotspot SSID: **`Zügli-Setup`**
- mDNS hostname: **`zugli.local`** (ASCII only — the `ü` appears only in display copy;
  see §3.3 for the umlaut caveat)
- UI copy: "Where should Zügli look?", "Save to Zügli", etc.

> ⚠️ The attached **use-case diagrams still say "Tramly"** (e.g. "Tramly-Setup" hotspot).
> Treat the *name* in those diagrams as outdated; the *flow* is correct. Use **Zügli**
> everywhere.

---

## 1. What the system does (one paragraph)

The device is a headless ESP32-S3 driving a 64×64 HUB75 RGB LED matrix. On first
power-up it has no WiFi, so it opens its own hotspot and shows a captive portal where
the user picks their home WiFi and enters the password. Once on the home network, the
device serves a small configuration web page (the `index.html` whose states are in the
attached mockups). On that page the user searches for a stop, picks one live connection
(line + destination), and saves it. The selection is stored in the device's flash. From
then on, the firmware polls the Swiss transport API every 30 seconds and renders the
countdown to the next departure of that connection on the LED panel. Holding the BOOT
button for 3+ seconds wipes the saved WiFi and returns to the captive-portal setup.

---

## 2. System architecture & data flow

There are **three runtime phases**. The agent must implement all three.

```
            ┌──────────────────────── PHASE 1: PROVISIONING (UC1) ─────────────────────────┐
            │  No WiFi saved → device is its own Access Point "Zügli-Setup"                 │
            │  Captive portal: SoftAP + DNS-catch-all + HTTP server on 192.168.4.1          │
            │  User picks home SSID + password → device saves creds to NVS → reboots        │
            └──────────────────────────────────────────────────────────────────────────────┘
                                              │  (creds valid)
                                              ▼
            ┌──────────────────────── PHASE 2: CONFIGURATION (UC2) ────────────────────────┐
            │  Device joined home WiFi (STA mode). Serves index.html at http://zugli.local  │
            │                                                                               │
            │  Phone browser ──fetch()──► transport.opendata.ch   (CORS, direct, no ESP)    │
            │     • GET /v1/locations?query=…   → stop autocomplete                         │
            │     • GET /v1/stationboard?id=…   → live connections at that stop             │
            │  Phone browser ──POST /save──► ESP32   (only the final small selection)       │
            └──────────────────────────────────────────────────────────────────────────────┘
                                              │  (selection saved)
                                              ▼
            ┌──────────────────────── PHASE 3: RUNTIME / DISPLAY ──────────────────────────┐
            │  Every 30 s the FIRMWARE itself calls:                                        │
            │     GET https://transport.opendata.ch/v1/stationboard?id=<stop>&limit=20      │
            │  Filters to the saved line+destination, keeps the next 3 departures,          │
            │  computes minutes-to-departure for each, renders on the 64×64 HUB75 panel.    │
            │                                                                               │
            │  BOOT held 3 s (UC3) → clear WiFi creds → reboot → Phase 1                     │
            └──────────────────────────────────────────────────────────────────────────────┘
```

**Key architectural decision (already settled, keep it):** the phone's browser talks to
the transport API *directly* for search/autocomplete — the ESP32 does **not** proxy those
calls. The ESP32 only (a) stores the final selection and (b) does its own polling at
runtime. This keeps the ESP32's job tiny (store a few strings, serve one static page,
poll one endpoint) and offloads all the heavy JSON/autocomplete work to the phone, which
has far more RAM. `transport.opendata.ch` has CORS enabled and needs no API key, so a
plain `fetch()` from the served page works.

> **Mixed-content note:** the page is served over **HTTP** from the ESP, and it calls the
> API over **HTTPS**. That direction is allowed by browsers (HTTPS subresource on an HTTP
> page). The reverse would be blocked. No issue here.

---

## 3. Hardware reference (context for the firmware)

The firmware targets this exact build. Pin numbers below are the **defaults the agent
should use**; they match the `esp-hub75` ESP32-S3 example and can be moved if a conflict
appears.

| Component | Detail |
|---|---|
| MCU | **ESP32-S3-DevKitC** (N16R8: 16 MB flash, 8 MB PSRAM), dual-core 240 MHz, 2.4 GHz WiFi only |
| Display | **64×64 HUB75** RGB panel (P3). **Confirm the exact build** — a single native 64×64 panel (1/32-scan, HUB75E) vs. two 64×32 panels stacked to 64×64; the two differ in scan/chain config (see §3.2) |
| Power | 5 V / 5 A PSU into the panel(s) (not via USB); **common ground** between ESP32 and panel is mandatory |
| Logic levels | ESP32 is 3.3 V, HUB75 wants 5 V logic → a **74HCT245 level shifter is recommended** (try without first; add if flicker/colour glitches) |
| Reset button | On-board **BOOT button = GPIO0**; usable as a normal input after boot (used for WiFi reset, UC3) |

### 3.1 HUB75 pin map (ESP32-S3, `Hub75Pins16` direct-drive, from the esp-hub75 example)

```
R1=GPIO38  G1=GPIO42  B1=GPIO48
R2=GPIO47  G2=GPIO2   B2=GPIO21
A=GPIO14   B=GPIO46   C=GPIO13   D=GPIO9   E=GPIO3
CLK=GPIO12 LAT=GPIO10 OE/BLANK=GPIO11
```
Note: a **64×64** panel is **1/32-scan** and **uses all five address lines A–E** (5 bits
→ 32 scan rows, each driving 2 of the 64 rows). The E line (GPIO3 above) is therefore
**required** here — make sure it's wired to the panel's E pin (the "E" in HUB75**E**).

### 3.2 Panel build / chaining (confirm which you have)

The firmware targets a single **64×64** framebuffer. Two ways to get there, and they
configure differently — **confirm which applies**:

- **Single native 64×64 HUB75E panel** — straightforward: one 1/32-scan panel, A–E used,
  framebuffer = 64×64. *(Recommended; simplest.)*
- **Two 64×32 panels arranged as 64×64** — note that chaining two 64×32 panels with the
  ribbon cable physically produces **128×32**, not 64×64. Presenting that as a 64×64
  image requires a panel-mapping/virtual-display configuration in the driver. If this is
  the build, flag it — the `esp-hub75` setup (rows/cols + scan) must be configured
  accordingly, and it's more fiddly than a native 64×64.

### 3.3 mDNS / how the user reaches the page

After joining home WiFi the device should advertise mDNS so the user can browse to a
name instead of hunting for an IP. **Use an ASCII hostname** — `zugli.local` — because
the `ü` in "Zügli" would require punycode (`xn--zgly-…`) in mDNS and is unreliable across
phones. If mDNS proves flaky on the user's network, fall back to instructing them to use
the device IP (the firmware should log/serve it). The captive portal's success screen
should tell the user the exact URL to open.

---

## 4. PART A — Configuration web page (`index.html`)

A **single self-contained `index.html`** — all CSS and JS inline, no build step, no
external requests except the transport API calls. This is the page served in **Phase 2**.
It is one HTML file that toggles between states with JS (no real page navigation).

The four attached state mockups (`initial_state`, `location_search`,
`connection_showcase`, `connection_selected`) are the source of truth for layout.

### 4.1 Brand tokens (extracted from the mockups — use these exact values)

> These supersede any older brand notes. The accent in the actual designs is a **muted
> copper**, not a bright amber.

```css
:root{
  --bg:        #191919; /* page background                                   */
  --surface:   #0E0C0A; /* connection rows, display-preview panel            */
  --accent:    #B87648; /* logo wordmark, badges, button, selected border,
                           display-preview text                              */
  --selected:  #2C2213; /* selected connection-row background (accent tint)  */
  --cream:     #F5EFE6; /* input field bg, headings, destination text        */
  --muted:     #5C554C; /* placeholder & secondary text                      */
}
```

**Typeface:** **Archivo** (single family for everything). Load via Google Fonts `<link>`
*or*, to keep the page fully offline-capable when served from the ESP, embed a subset as
base64 `@font-face` (recommended — the page must render even though the phone reaches the
API over the internet, the *page itself* comes from the ESP with no internet guarantee
for font CDNs). Weights & sizes:

| Role | Weight | Size |
|---|---|---|
| H1 wordmark "Zügli" | Bold 700 | 30 px |
| H2 section labels | Medium 500 | 19 px |
| Body / list text | Regular 400 | 16 px |
| Button label | SemiBold 600 | 19 px |

Layout is a single centered column, generous spacing, left-aligned content, dark page.

### 4.2 The four states

**State 1 — Initial** (`initial_state`)
- H1 "Zügli" (accent colour).
- H2 "Where should Zügli look?" (cream).
- One text input, cream background, placeholder "Search a stop…" (muted).

**State 2 — Location search** (`location_search`)
- As the user types (debounce ~250 ms, min 2 chars), an autocomplete dropdown appears
  directly below the input — cream panel, dark text, thin dividers between rows.
- Rows are stop names from the API, e.g. "Zürich, Letzibach", "Zürich, Letzistrasse",
  "Zug, Letzi". Tapping a row selects that stop and collapses the dropdown.

**State 3 — Connection showcase** (`connection_showcase`)
- The input now shows the chosen stop (e.g. "Zürich, Letzigrund").
- A new H2 "Which connection?" appears.
- Below it, a list of connection rows on dark surface (`--surface`). Each row =
  **[line badge] → [destination]**. Examples from the mockup: `2 → Klausplatz`,
  `2 → Schlieren`, `S123 → Brugg`, `S123 → Rapperswil-Jona`.
- **Badge styling depends on transport type:**
  - **Tram / bus** (short numeric lines like `2`): **filled** accent badge, dark text.
  - **S-Bahn / train** (lines like `S123`): **outlined** accent badge, accent text.
  - (Derive from the API `category` field — see §6.)
- The arrow `→` and destination text are cream/white.

**State 4 — Connection selected** (`connection_selected`)
- The tapped row gets an **accent border** and a **`--selected` background tint**.
- A preview section appears lower on the page:
  - Small H2 "This is what Zügli will show".
  - A **display-preview panel** on `--surface`, styled like the real LED board:
    accent text on near-black, showing `‹line› ‹destination›` left-aligned and
    `‹minutes›'` right-aligned. Mockup example: `2 Schlieren …………… 11'`.
  - This preview should use the same minutes value the device will show, fetched live
    from the stationboard for that connection (so the user sees a real countdown).
- A full-width **"Save to Zügli"** button (filled accent, dark label) at the bottom.

### 4.3 Missing state the agent must add: **Save in-progress / success**

The mockups don't include the post-save feedback, but the flow (UC2: "transmit button
runs an animation while everything is sent") requires it. Implement:

1. **Saving** — on tap, the button enters a loading state (spinner or animated label,
   disabled). Copy suggestion: "Saving…".
2. **Success** — on `200 OK` from the ESP, show a confirmation. Copy suggestion:
   **"Saved — Zügli is updating."**
3. **Error** — on network failure / non-200, re-enable the button and show an inline
   error ("Couldn't reach Zügli — try again.").

### 4.4 What the page POSTs to the device

When the user taps Save, POST a **small JSON body** to the ESP — only the final
selection, nothing more:

```
POST /save   (Content-Type: application/json)
{
  "stopId":      "8591273",            // the API location `id` of the chosen stop
  "stopName":    "Zürich, Letzigrund", // for display/echo only
  "line":        "2",                  // the line number/name (API `number`)
  "category":    "T",                  // raw API category (drives badge + future styling)
  "destination": "Schlieren"           // the API `to` field of the chosen departure
}
```

The device stores these fields and replies `200 OK` (e.g. `{"ok":true}`). The
combination **(stopId + line + destination)** is the unique key the firmware filters on
at runtime.

### 4.5 Page logic summary (for the implementer)

- Single HTML file, inline `<style>` and `<script>`, no frameworks needed (vanilla JS).
- State held in plain JS variables; no `localStorage`/`sessionStorage`.
- API calls: see §6 for exact endpoints, params, and response fields.
- Graceful handling of: no results, API timeout, a stop with no current departures.
- Keep it phone-first (narrow viewport), touch targets ≥ 44 px.

---

## 5. PART B — Provisioning / captive portal (Phase 1, UC1)

Triggered when **no valid WiFi credentials are stored**, or after a WiFi reset (UC3), or
when stored credentials fail to connect.

### 5.1 Flow (from UC1 diagram, name corrected to Zügli)

1. Power on, no WiFi saved → device starts **SoftAP** broadcasting **`Zügli-Setup`**
   (open network, or WPA2 with a printed/known password — open is simpler for a school
   project; confirm preference).
2. User connects their phone to that hotspot.
3. A **captive portal** auto-opens (the OS connectivity check is redirected to the
   device's page). If it doesn't auto-open, the user can browse to `http://192.168.4.1`.
4. Page shows a **list of nearby WiFi networks** (from an AP scan) + a **password field**.
5. User picks their network, enters the password, submits.
6. Device attempts to join.
   - **Wrong credentials → show an error and return to step 4** (loop, per the diagram).
   - **Success → save credentials to NVS flash, reboot into STA mode (Phase 2).**
7. After reboot the device joins home WiFi and serves the config page. The portal's
   success screen should tell the user to reconnect their phone to their **home** WiFi and
   open **`http://zugli.local`** (there is no seamless cross-network auto-redirect — the
   user changes networks manually).

### 5.2 Captive portal mechanics (three services on the SoftAP)

- **Access Point** at `192.168.4.1`.
- **DNS catch-all**: a tiny UDP DNS server on port 53 that answers *every* query with
  `192.168.4.1`, so the phone's captivity check resolves to the device. Also handle the
  common probe paths (`/generate_204`, `/hotspot-detect.html`, `/ncsi.txt`, etc.) by
  returning the portal so iOS/Android/Windows all pop the portal.
- **HTTP server** serving the WiFi-setup page and accepting the submitted credentials.

### 5.3 Design for the setup page — **delivered**

A brand-consistent setup page has been designed and is delivered alongside this brief as
**`designs/zugli_setup_page.html`** — a self-contained mockup (inline CSS/JS, mock data) in the
Zügli identity (tokens from §4.1, Archivo, dark bg, copper accent). It mirrors the config
page's language: H1 "Zügli" wordmark, an H2 "Which network?" step, a cream network list
styled like the config page's autocomplete dropdown (each row shows SSID + lock + signal
bars), then a password field and a copper **"Connect"** button.

It implements all the states the captive flow needs:

1. **Network list** — scanned SSIDs (replace mock data with the ESP's AP-scan results).
2. **Password** — chosen SSID shown, password input, Connect.
3. **Connecting** — button in loading state while the ESP attempts to join.
4. **Wrong password** — inline error, returns to the password step (the UC1 loop).
5. **Success** — directs the user to reconnect their phone to home WiFi and open
   `zugli.local`.

The file includes a small **preview switcher** at the bottom (so all states can be
viewed); **remove that block for production**. Wire-up notes for the implementer are in
inline comments: the network list comes from the ESP's scan, and Connect should POST
`{ssid, password}` to the ESP and await its real connect result rather than the mock
timer.

---

## 6. Transport API reference (`transport.opendata.ch`, v1)

Public Swiss transport API. **No auth, CORS enabled.** Used by both the phone (Phase 2)
and the firmware (Phase 3). Be mindful of fair-use rate limits — the 30 s firmware poll
is well within them.

### 6.1 Stop autocomplete (phone only)

```
GET https://transport.opendata.ch/v1/locations?query=<typed text>&type=station
```
Returns `stations[]`; each has `id`, `name`. Use `id` as `stopId`, `name` for display.

### 6.2 Live connections at a stop (phone + firmware)

```
GET https://transport.opendata.ch/v1/stationboard?id=<stopId>&limit=20
```
Returns `stationboard[]`. There is **no "list all lines" endpoint** — the available
connections are derived from this board. Relevant fields per entry:

| Field | Meaning | Used for |
|---|---|---|
| `category` | `T` tram, `B` bus, `S` S-Bahn, `IR`/`IC`… train | badge style (filled vs outlined) |
| `number` | line number/name, e.g. `2`, `S12` | the `line` value, badge label |
| `to` | final destination, e.g. `Schlieren` | the `destination` value |
| `stop.departureTimestamp` | **Unix epoch seconds** of departure | minutes-to-departure |
| `stop.prognosis.departure` | real-time delayed time if present | optional: prefer over scheduled |

**Building the connection list (Phase 2, State 3):** fetch the board with `limit=20`,
then **de-duplicate by (number + to)** so each distinct connection appears once in the
picker.

**Selecting the next departures (Phase 3, firmware):** for the *saved* connection, do
**not** de-duplicate — instead keep the **next 3 entries** matching the saved
`(number == line, to == destination)`, sorted by `departureTimestamp` ascending. These
three feed the display. (If fewer than 3 match within `limit=20`, raise the limit or show
what's available.)

**Computing minutes (phone preview and firmware, per departure):** use the **real-time
time when available, else the scheduled time** — i.e. prefer `stop.prognosis.departure`
(live/delayed) and fall back to `stop.departureTimestamp`. Then
`minutes = max(0, round((chosen_departure − now_unix) / 60))`.
Because these are absolute Unix times, **no timezone math is needed** — just compare
against the current Unix time. The firmware gets `now_unix` from SNTP (§7.4). *(Note:
`prognosis.departure` is an ISO datetime; `departureTimestamp` is epoch seconds — normalise
both to epoch seconds before subtracting.)*

---

## 7. PART C — Device firmware (Rust, `no_std`)

The firmware is **embedded Rust on bare metal** (`no_std`, `esp-hal`), async via
**Embassy**. The sections below give the agent a verified, mutually-compatible crate
stack and the task breakdown. **All crates below were checked for compatibility for this
exact use (ESP32-S3, WiFi + HUB75 together).**

### 7.1 Verified crate stack

| Concern | Crate | Notes |
|---|---|---|
| HAL | **`esp-hal`** | no_std HAL for ESP32-S3 |
| Async runtime | **`embassy-executor`** + **`embassy-time`** | dual-core friendly |
| WiFi driver | **`esp-wifi`** (a.k.a. `esp-radio` in newest releases) | STA **and** SoftAP modes |
| TCP/IP stack | **`embassy-net`** (+ `smoltcp`) | needs `tcp`, `udp`, `dns`, `dhcpv4` features |
| HTTP server (config + portal) | **`picoserve`** | async no_std HTTP server, embassy-native |
| HTTP client (API poll) | **`reqwless`** | no_std HTTP/HTTPS client |
| TLS (for HTTPS API) | **`embedded-tls`** *or* **`esp-mbedtls`** | see §7.5 — this is the main TLS decision |
| Captive DNS | small custom UDP responder *or* an `edge-captive`-style helper | answer every query with `192.168.4.1` |
| Display driver | **`esp-hub75`** (liebman) | DMA HUB75 driver, **built on embedded-graphics** |
| Graphics | **`embedded-graphics` 0.8** | primitives + text |
| Fonts | **`u8g2-fonts`** (optional) | nicer/larger fonts than the built-in mono fonts |
| Persistent storage | **`esp-storage`** + **`sequential-storage`** (or NVS) | store WiFi creds + selection in flash |
| Time | **`sntpc`** (or esp SNTP) | get Unix time for minute math |

> **Scaffold with `esp-generate`.** The ESP-Rust ecosystem moves fast and crate versions
> must agree (esp-hal ↔ esp-wifi ↔ embassy ↔ esp-hub75). Start the project with
> `esp-generate` (select: ESP32-S3, alloc, unstable HAL, WiFi, Embassy) so the base
> versions are coherent, then add `esp-hub75`, `picoserve`, `reqwless`, and storage on
> top. **Pin versions from a single working commit; don't hand-pick mismatched versions.**

### 7.2 `esp-hub75` specifics (confirmed from its docs)

- Use **`Hub75::new_async(...)`** on the ESP32-S3's **LCD/CAM peripheral** (I8080 mode).
- Use a **bitplane direct-drive framebuffer**
  (`framebuffer::bitplane::plain::DmaFrameBuffer`) — strongly recommended for low RAM.
- Pin config: **`Hub75Pins16`** (direct drive; matches a standard Waveshare panel).
- Initialise with a pixel clock around **`Rate::from_mhz(20)`** (tune for flicker).
- **Enable the `iram` feature.** It places the render/DMA hot-path in IRAM to avoid
  flash-cache stalls during WiFi/flash activity — directly mitigates the WiFi+HUB75
  flicker risk (§7.6). Costs ~5–10 KiB IRAM.
- It implements `embedded-graphics` `DrawTarget`, so text/shapes draw normally.

### 7.3 Async task breakdown (Embassy)

Recommended tasks, with the **display render loop pinned to one core** and WiFi/network
on the other (see §7.6):

1. **`net_task`** — runs the `embassy-net` stack.
2. **`provisioning_task`** (Phase 1 only) — SoftAP + DNS catch-all + `picoserve` setup
   page; on valid creds, persist + reboot.
3. **`config_server_task`** (Phase 2) — `picoserve` serving `index.html` + `POST /save`;
   on save, persist selection.
4. **`poll_task`** (Phase 3) — every **30 s**: `reqwless` GET the stationboard for the
   saved `stopId`, filter to `(line, destination)`, keep the **next 3** by departure time,
   compute minutes for each, push the result (up to 3 entries) into a shared state cell
   (e.g. an `embassy_sync::signal::Signal` or `Mutex`).
5. **`render_task`** (Phase 3, **pinned to the second core**) — continuously refreshes the
   HUB75 framebuffer from shared state and drives the DMA; redraws text when the value
   changes.
6. **`button_task`** — polls GPIO0; on a **3 s hold**, clear WiFi creds and reboot (UC3).

### 7.4 Time sync

After joining WiFi, sync time once via **SNTP** (`sntpc`) and refresh periodically. The
poll task uses this Unix time to compute `minutes = (departureTimestamp − now)/60`.

### 7.5 TLS decision (the one real choice to make)

The API is **HTTPS-only**, so the firmware needs TLS for its outbound poll. Two options,
both work with `reqwless`:

- **`embedded-tls`** — pure Rust, released on crates.io, simplest to wire up.
  **Caveat:** certificate verification is *not supported* in no_std, so you run with
  `TlsVerify::None`. Acceptable for a hobby/school device on a trusted network; document it.
- **`esp-mbedtls`** — Espressif's mbedTLS wrapper with **hardware acceleration** and real
  cert verification, but it's a **git dependency** (not yet on crates.io) and needs
  `alloc`. More setup, more robust.

**Recommendation:** start with **`embedded-tls` + `TlsVerify::None`** to get end-to-end
working, then optionally harden to `esp-mbedtls` later. (If TLS proves painful, a fallback
is a tiny relay, but that defeats the "no backend" goal — avoid unless necessary.)

### 7.6 ⚠️ Primary implementation risk: WiFi + HUB75 DMA together

This is the single hardest part and the agent should plan for it explicitly. Driving a
HUB75 panel needs tight, continuous DMA timing; WiFi has its own real-time demands and
shares the bus/CPU. Symptoms of conflict are **visible flicker or colour glitches** during
network activity. Mitigations, in order:

1. **`iram` feature** on `esp-hub75` (§7.2) — biggest single win.
2. **Pin the render loop to the second core**; keep WiFi + networking on the first.
3. Keep network bursts short (the 30 s poll is brief; the display is otherwise idle).
4. Add the **74HCT245 level shifter** if glitches look electrical rather than timing.
5. Tune the HUB75 pixel clock.

### 7.7 Display content & layout (Phase 3)

The panel is **64×64**. The data layer provides the **next 3 departures** of the saved
connection (line + destination + minutes each, soonest first).

> **Layout is intentionally left open for now — implement a clear PLACEHOLDER, not the
> final design.** The visual treatment for 64×64 hasn't been decided; the user wants to
> see how three departures look on real hardware before committing. So:
>
> - Build a **`render_departures(&[Departure])` placeholder** that stacks up to 3 rows
>   top-to-bottom, each `‹line› ‹destination›` left / `‹minutes›'` right, accent on black,
>   in a legible `embedded-graphics` mono font (`FONT_5X7`/`FONT_6X10`, or `u8g2-fonts`
>   for something nicer). 64 px tall comfortably fits 3 rows.
> - Keep this rendering **isolated behind one function** so the layout can be reworked
>   later without touching polling/state/networking code.
> - The config-page preview (State 4) still shows a single line as its example — that's
>   fine; the *device* layout is a separate, later decision.

Data shape the render function receives (from `poll_task`):

```
Departure { line: "2", category: "T", destination: "Schlieren", minutes: 11 }
// up to 3, sorted soonest-first
```

**Required runtime display states (edge cases the firmware must handle):**

| Situation | Suggested placeholder display |
|---|---|
| Normal (≥1 departure) | up to 3 rows: `2 Schlieren 11'` / `2 Schlieren 23'` / `2 Schlieren 35'` |
| Departing now | show `0'` or `now` for that row |
| Fewer than 3 matches | show however many matched |
| No matching departure on the board | one row: `2 Schlieren --` (or "no service") |
| API unreachable / poll failed | keep last values briefly, then a subtle "offline" indicator; retry next cycle |
| WiFi lost | reconnect attempts; small disconnected glyph; if creds invalid, fall to Phase 1 |
| Booting / no selection yet | a "Zügli" splash or "Open zugli.local to set up" hint |

Colour: render text in the accent copper (`#B87648`) on black to mirror the design. On an
RGB panel set the accent as an explicit `Rgb888`/`Rgb565` value — do **not** rely on a
generic "yellow" constant, since the brand colour is a specific copper tone.

### 7.8 Persistence (NVS / flash)

Store two records in flash so they survive reboots and are managed independently:

- **WiFi credentials** — written in Phase 1; **cleared by UC3** (BOOT 3 s hold).
- **Connection selection** — the `/save` payload from §4.4; written in Phase 2.

On boot: if WiFi creds exist → try STA (Phase 2/3); else → Phase 1. If a selection exists →
start polling/rendering; else → idle splash prompting setup.

### 7.9 BOOT-button WiFi reset (UC3)

`button_task` reads GPIO0. On a continuous **3 s hold**: clear the stored WiFi credentials
**only** (leave the saved connection selection intact), then **reboot**. The device comes
back up with no WiFi → Phase 1 captive portal; once re-joined to a network it resumes the
same connection. (GPIO0 is the BOOT strapping pin; it's a normal input after boot, so this
is purely a software behaviour.)

---

## 8. Decisions (all settled)

Everything below is decided — build to these, no further confirmation needed.

- ✅ **Name → Zügli.** (§0)
- ✅ **Captive-portal page → designed and delivered** as `designs/zugli_setup_page.html`. (§5.3)
- ✅ **Display content → next 3 departures** of the saved connection; the on-panel *visual
  layout* stays a placeholder for now, to be refined on real hardware. (§7.7)
- ✅ **(1) Panel build → single native 64×64 HUB75E panel.** Configure `esp-hub75` for one
  1/32-scan 64×64 panel (address lines A–E, E required). No virtual-panel remapping. (§3.1–3.2)
- ✅ **(2) Setup hotspot → open network** (no password). `Zügli-Setup` is open; users tap to
  join. (§5.1)
- ✅ **(3) Device address → `zugli.local` (mDNS), with the raw IP shown as a fallback.** The
  firmware must also display/log its IP, and the setup success screen should show both the
  `zugli.local` name and the IP, in case `.local` is flaky on the user's network. (§3.3, §5.1)
- ✅ **(4) TLS → `embedded-tls` with `TlsVerify::None`** (no certificate verification) for the
  outbound API poll. Document the trade-off in the README. Hardening to `esp-mbedtls` is a
  possible future step, not required now. (§7.5)
- ✅ **(5) BOOT reset → clears WiFi credentials only**, leaving the saved connection intact, so
  the device resumes the same connection after re-joining a network. (§7.9)
- ✅ **(6) Departure times → real-time when available, else scheduled.** Use
  `stop.prognosis.departure` (live/delayed) when present; otherwise `stop.departureTimestamp`.
  Apply this in both the phone preview and the firmware. (§6.2)
- ✅ **(7) Font → embed Archivo** in the page (base64 `@font-face`), so it always renders
  correctly when served offline by the ESP. No CDN dependency in production. (§4.1)

---

## 9. Build order (suggested)

1. **`index.html` standalone** — build and test the four states + save flow in a desktop
   browser against the live API, with the POST pointed at a mock endpoint. (Fastest
   feedback, no hardware needed.)
2. **Firmware skeleton** via `esp-generate` — WiFi STA + `picoserve` serving the
   `index.html` + `/save` writing to flash. Verify Phase 2 end-to-end.
3. **Poll + display** — `reqwless` poll → shared state → `esp-hub75` render of one
   connection. Tackle the WiFi+HUB75 flicker risk here (§7.6).
4. **Provisioning** — SoftAP + DNS catch-all + setup page (Phase 1), then the boot
   logic that chooses Phase 1 vs 2/3.
5. **BOOT reset** (UC3) + all edge-case display states (§7.7).

---

## 10. Repository & workflow conventions

**Repository:** https://github.com/AarKro/zugli

**Repository layout (target):**

```
zugli/
├─ zugli_project_brief.md      # this brief, in the project root
├─ README.md                    # keep updated (see below)
├─ .gitignore                   # the agent must create this (see below)
├─ designs/                     # design assets
│   ├─ initial_state.svg
│   ├─ location_search.svg
│   ├─ connection_showcase.svg
│   ├─ connection_selected.svg
│   ├─ UC1__First-time_WiFi_setup.svg
│   ├─ UC2__Configure_stop___line.svg
│   ├─ UC3__Reset_saved_WiFi.svg
│   └─ zugli_setup_page.html   # captive-portal setup page (also the impl. starting point)
├─ web/                         # the served config index.html (Phase 2) lives here
└─ firmware/                    # the Rust no_std firmware crate
```
*(The `designs/` folder and the brief in root already exist / are user-managed; the agent
creates `web/`, `firmware/`, `README.md`, and `.gitignore`.)*

**`.gitignore` — the agent must write one** appropriate to the stack. At minimum it should
cover the Rust/embedded build and common cruft:

- Rust: `/target/`, `**/target/`, `Cargo.lock` *(keep `Cargo.lock` for a binary/firmware
  crate — do **not** ignore it; only ignore it for libraries)*, `*.rs.bk`
- Embedded/ESP: build artifacts, `.embuild/`, flashing logs, any `*.bin`/`*.elf` output dirs
- Editor/OS: `.vscode/` (or keep a shared subset), `.idea/`, `.DS_Store`, `Thumbs.db`
- Secrets/env: `.env`, anything holding WiFi creds or local config (these must never be
  committed)

**`README.md` — keep it updated.** The agent should maintain a real README that covers:
what Zügli is (one-paragraph intro), the hardware needed, the repo layout, how to build &
flash the firmware, how to run/serve the config page, the first-time setup flow (UC1→UC2),
how to reset WiFi (UC3), and the **security note** that the firmware uses TLS without
certificate verification (decision §8-4). Update it as features land — don't leave it stale.

**Commits — make regular, sensible commits.** The agent should commit at meaningful
checkpoints rather than one giant commit at the end: e.g. after the standalone `index.html`
works, after the firmware skeleton serves the page, after polling+display renders, after
provisioning works, after the BOOT reset, etc. (roughly mirroring the build order in §9).
Use clear, conventional commit messages (e.g. `feat: serve config page over picoserve`,
`fix: hub75 flicker during wifi poll`, `docs: update README setup flow`). Commit working
increments; avoid committing secrets or build artifacts (covered by `.gitignore`).

---

## Appendix — design files (in `designs/`)

All design assets live in the **`designs/`** folder of the repo. This brief lives in the
**project root**.

| File | Phase | What it shows |
|---|---|---|
| `designs/initial_state.svg` | 2 | Config page, empty search |
| `designs/location_search.svg` | 2 | Stop autocomplete dropdown |
| `designs/connection_showcase.svg` | 2 | Connection list with badges |
| `designs/connection_selected.svg` | 2 | Selected row + live display preview + Save button |
| `designs/UC1__First-time_WiFi_setup.svg` | 1 | Captive-portal provisioning flow |
| `designs/UC2__Configure_stop___line.svg` | 2 | Stop/line configuration flow |
| `designs/UC3__Reset_saved_WiFi.svg` | 3 | BOOT-button WiFi reset flow |
| `designs/zugli_setup_page.html` | 1 | Captive-portal WiFi setup page — interactive mockup in the Zügli brand, all states (network list / password / connecting / wrong-password / success). Doubles as the implementation starting point. |

*Note: text in the design SVGs is outlined (converted to paths), so the typeface (Archivo)
and copy are documented above rather than extractable from the files.*
