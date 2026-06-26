# Zügli — Assembly & Software Loading Guide

A step-by-step guide to building the Zügli transit board hardware and loading the
firmware + config page onto it. Follow the parts in order: **gather parts → wire the
panel → power it → install the toolchain → flash the firmware → first-time setup.**

> This guide is the practical "how to build one" companion to
> [`PROJECT_BRIEF.md`](PROJECT_BRIEF.md). When a value here (a pin, a crate,
> a decision) needs justification, the brief section is cited like **(brief §3.1)**.

---

## Part 0 — Before you start

**You do not need the board attached to write or compile-check firmware** — `cargo build`
works on its own (brief §11). You only need the hardware for the flashing and on-device
steps (Parts 2–6). If you just want to verify code, skip to Part 3 and run `cargo build`.

**Safety first (read once):**

- Always wire and re-wire with **power disconnected**. Only apply 5 V after you've
  double-checked every connection.
- **Common ground is mandatory** — the ESP32 ground and the panel/PSU ground must be tied
  together, or the panel will show garbage or nothing (brief §3).
- **Do not power the LED panel from the ESP32's USB.** A 64×64 panel can pull several amps;
  USB can't supply that. Use the dedicated 5 V / 5 A PSU (brief §3).
- Observe PSU polarity. Reversing 5 V and GND into the panel can destroy it.

---

## Part 1 — Bill of materials (gather these)

| # | Part | Spec / note | Brief ref |
|---|------|-------------|-----------|
| 1 | **ESP32-S3-DevKitC** | N16R8 variant (16 MB flash, 8 MB PSRAM). Has the on-board **BOOT** button (GPIO0). | §3 |
| 2 | **64×64 HUB75E RGB LED panel** | Single **native** 64×64, P3 pitch, **1/32-scan**, address lines **A–E** (the "E" matters). Not two chained 64×32 panels. | §3, §3.2 |
| 3 | **5 V / 5 A DC power supply** | Powers the panel directly. 5 A gives headroom for a bright 64×64. | §3 |
| 4 | **HUB75 power pigtail** | The 2-pin/4-pin spade or screw-terminal lead that ships with the panel, from PSU to the panel's `+5V`/`GND` lugs. | §3 |
| 5 | **Jumper wires** | ~16 female-female Dupont leads for the 14 signal lines + ground below. Keep them short to reduce glitches. | §3.1 |
| 6 | **USB-C cable** | Data-capable (not charge-only), ESP32-S3 ↔ computer, for flashing + serial. | §11 |
| 7 | *(optional)* **74HCT245 level shifter** | Only add if you see flicker/colour glitches — try without it first. | §3, §7.6 |
| 8 | *(optional)* breadboard / protoboard | To host the 74HCT245 and tidy wiring if used. | — |

> The HUB75 input connector is a 2×8 (16-pin) IDC header. Panels usually have an **IN** and
> an **OUT** side — connect to **IN** (look for the arrow / "J1" / "IN" silkscreen).

---

## Part 2 — Wire the panel to the ESP32

These are the **default pins the firmware uses** (brief §3.1). They match the `esp-hub75`
ESP32-S3 example with `Hub75Pins16` direct-drive. If you change a wire, you must change the
matching pin constant in the firmware — so it's easiest to wire exactly as below.

### 2.1 Signal lines (ESP32-S3 GPIO → HUB75 IN header)

| HUB75 pin | Meaning | ESP32-S3 GPIO |
|-----------|---------|---------------|
| R1 | upper-half red | **GPIO38** |
| G1 | upper-half green | **GPIO42** |
| B1 | upper-half blue | **GPIO48** |
| R2 | lower-half red | **GPIO47** |
| G2 | lower-half green | **GPIO2** |
| B2 | lower-half blue | **GPIO21** |
| A | address bit 0 | **GPIO14** |
| B | address bit 1 | **GPIO46** |
| C | address bit 2 | **GPIO13** |
| D (GND) | address bit 3 | **GPIO9** |
| **E** | address bit 4 | **GPIO3** |
| CLK | pixel clock | **GPIO12** |
| LAT (STB) | latch | **GPIO10** |
| OE (BLANK) | output enable | **GPIO11** |
| GND | ground | **any ESP32 GND** |

> **The E line (GPIO3) is required.** A native 64×64 panel is 1/32-scan and uses all five
> address lines A–E. Skipping E is the classic "only half / wrong rows light up" mistake
> (brief §3.1). Make sure GPIO3 reaches the panel's **E** pad.

On the 2×8 header, the panel silkscreens each pad (R1 G1 / R2 G2 / B1 B2 / A B / C D / CLK
LAT / OE GND / E …). Match by **label, not by physical position** — header pinouts vary
slightly between panel vendors. The E pad is sometimes where an older 1/16-scan panel had a
second GND.

### 2.2 Ground (do not skip)

Tie **at least one HUB75 GND pin on the header to an ESP32 GND pin**. This is the signal
common ground and is separate from the high-current power ground in Part 3. Both grounds
ultimately connect, which is exactly what you want — **common ground is mandatory**
(brief §3).

### 2.3 Optional 74HCT245 level shifter

The ESP32 drives 3.3 V logic; HUB75 panels expect 5 V logic. Many panels work fine on 3.3 V
directly — **try without the shifter first** (brief §3, §7.6). If you get flicker or wrong
colours that look *electrical* (present even when WiFi is idle), insert a **74HCT245**
between the ESP32 and the panel on the 14 output signals (R1 G1 B1 R2 G2 B2 A B C D E CLK
LAT OE), powered from 5 V, with its direction pin tied for ESP→panel flow and `OE` to GND.

---

## Part 3 — Power and a smoke test

1. With **everything unpowered**, connect the **5 V / 5 A PSU** to the panel's power lugs
   (`+5V` and `GND`), watching polarity (brief §3).
2. Connect the **USB-C** cable from the ESP32-S3 to your computer (this powers only the
   ESP32 logic, not the panel).
3. Re-check: signal wires match Part 2.1, the **E** line is connected, and a header **GND**
   ties to an ESP32 **GND** (Part 2.2).
4. Power on the PSU. At this stage, with no firmware, the panel may show random pixels or
   stay dark — that's fine. Nothing should get hot. If anything heats up or smells, kill
   power immediately and recheck polarity/grounds.

You'll see the panel actually render after flashing (Part 5) and configuring (Part 6).

---

## Part 4 — Install the build toolchain (one time)

The ESP32-S3 is **Xtensa** architecture and needs a special Rust toolchain (a fork), not
stock Rust. Because the firmware is `no_std` and uses `embedded-tls` (not `esp-mbedtls`),
the full ESP-IDF C SDK is **not** required — `espup` provides everything (brief §11).

The brief documents macOS; the commands below cover **macOS/Linux** and **Windows**, since
this repo is checked out on Windows.

### 4.1 macOS / Linux

```bash
# macOS only: Xcode command-line tools (git + a C compiler)
xcode-select --install

# Rust via rustup — NOT a package-manager "fixed" Rust (it breaks the espup flow)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Xtensa toolchain for the ESP32-S3
cargo install espup --locked
espup install

# Flasher + serial monitor, and the no_std project scaffolder
cargo install espflash --locked
cargo install esp-generate
```

### 4.2 Windows (PowerShell)

```powershell
# Rust via rustup (download & run from https://rustup.rs if winget is unavailable)
winget install Rustlang.Rustup

# Xtensa toolchain for the ESP32-S3
cargo install espup --locked
espup install

# Flasher + serial monitor, and the no_std project scaffolder
cargo install espflash --locked
cargo install esp-generate
```

On Windows, `espup install` writes `%USERPROFILE%\export-esp.ps1`. You usually **don't need
to source it** — the generated `rust-toolchain.toml` selects the `esp` toolchain
automatically, and the `no_std` stack avoids the libclang/bindgen path (brief §11). Only if
you hit an error mentioning `libclang` or `unknown target triple 'xtensa'` do you need to
run that script in your shell first.

### 4.3 Verify the toolchain

```bash
rustup toolchain list      # should include "esp"
espflash --version         # flasher present
```

If `cargo` isn't found in a fresh shell, add `~/.cargo/bin` (Windows:
`%USERPROFILE%\.cargo\bin`) to your PATH.

---

## Part 5 — Build and flash the firmware

> Prerequisite: the `firmware/` crate **exists in this repo** and is the binary you flash.
> It was scaffolded with `esp-generate` (ESP32-S3, alloc, unstable HAL, WiFi, Embassy) and
> builds on the **`esp`** toolchain (pinned in `firmware/rust-toolchain.toml`), Rust
> **edition 2024**. The full dependency stack and versions are in `firmware/Cargo.toml` /
> `Cargo.lock` and documented in brief §7.1.

### 5.1 Compile-check (no board needed)

From the firmware crate:

```bash
cd firmware
cargo build          # or: cargo clippy
```

This compiles **without the board attached** and is the fastest way to confirm the code is
sound (brief §11).

### 5.2 Flash + monitor (board required)

1. Connect the ESP32-S3 to the computer over **USB-C** (the panel stays on its own PSU).
2. From `firmware/`:

   ```bash
   cargo run
   ```

   This **builds, flashes, and opens the serial monitor** in one step and **requires the
   board connected over USB** (brief §11).

3. Watch the serial log for boot output. On a fresh device with no saved WiFi, the firmware
   starts **Phase 1** — the captive portal (next part).

### 5.3 If the board isn't detected

- The S3-DevKitC has **two USB-C ports** — try the **other one** (one is USB-OTG, one is the
  UART bridge) (brief §11).
- Use a **data-capable** cable, not charge-only.
- Install the USB-serial driver if needed: Silicon Labs **CP210x** or **CH34x** (brief §11).
- As a last resort, force download mode: hold **BOOT**, tap **RESET**, release **BOOT**,
  then run `cargo run` again.

### 5.4 Where the config page lives

The Phase 2 config page (`web/index.html`) is **served by the firmware** — it's embedded
into the firmware binary and delivered when you flash, so there's no separate upload step
for it. The captive-portal setup page is likewise served by the device. You only flash the
firmware; the pages ride along with it.

---

## Part 6 — First-time setup (UC1 → UC2)

This is the on-device flow once flashed. No code, just a phone.

### 6.1 Connect to WiFi (Phase 1 — captive portal, UC1)

1. On first power-up with no saved WiFi, the device opens its own **open** hotspot named
   **`Zügli-Setup`** (no password) (brief §5.1).
2. On your phone, join `Zügli-Setup`. A **captive portal** should auto-open. If it doesn't,
   browse to **`http://192.168.4.1`** (brief §5.1).
3. The page lists nearby WiFi networks. **Pick your home network, enter its password,** tap
   **Connect**.
4. The device tries to join:
   - **Wrong password →** it shows an error and returns to the network step — try again
     (brief §5.1).
   - **Success →** it saves the credentials to flash and **reboots** into normal mode.

### 6.2 Find the device on your network

1. After it reboots, **reconnect your phone to your normal home WiFi** (the device won't be
   broadcasting `Zügli-Setup` anymore). There's no automatic cross-network redirect — you
   switch networks manually (brief §5.1).
2. Open **`http://zugli.local`** in your phone's browser (mDNS) (brief §3.3).
3. **If `zugli.local` doesn't resolve** (mDNS can be flaky on some phones/networks), look at
   the **LED panel** — while it has no connection selected yet, it displays the device's
   current **IP address** (e.g. `192.168.1.42`). Type **`http://<that IP>`** into the
   browser instead (brief §3.3, §7.7).

### 6.3 Pick your stop and connection (Phase 2 — UC2)

On the config page (brief §4.2):

1. **Search a stop** — start typing; an autocomplete dropdown shows matching Swiss stops.
   Tap the one you want.
2. **Which connection?** — a list of live connections (line badge → destination) appears.
   Tap the one you want to track.
3. A **preview** shows what the panel will display, with a live countdown.
4. Tap **"Save to Zügli."** The device stores the selection and **switches live with no
   reboot** — the panel starts showing the countdown within one 30 s poll cycle (brief §4.4).

The config page **stays reachable** at `zugli.local` the whole time the device runs, so you
can change the stop/line whenever you like without resetting anything (brief §2, §4.4).

---

## Part 7 — Resetting WiFi (UC3)

To move the device to a different network: **hold the BOOT button for 3+ seconds**. This
clears the stored WiFi credentials **only** — your saved stop/connection is kept — and the
device reboots back into the `Zügli-Setup` captive portal (Part 6.1). Once it rejoins a
network, it resumes the same connection (brief §7.9, §8-5).

---

## Part 8 — Troubleshooting

| Symptom | Likely cause / fix |
|---------|--------------------|
| Panel dark or random pixels after flashing | Check signal wiring (Part 2.1), and that header **GND** ties to ESP32 **GND** (Part 2.2). |
| Only half the panel, or wrong rows light up | The **E** line (GPIO3) isn't connected — a 64×64 panel needs all of A–E (brief §3.1). |
| Flicker / colour glitches **during** WiFi polls | Timing contention — ensure the firmware's `iram` feature is on and the render loop is pinned to the second core (brief §7.6). |
| Flicker / wrong colours even when idle | Looks electrical — add the **74HCT245** level shifter (Part 2.3) and/or tune the pixel clock (brief §7.6). |
| Board not detected when flashing | Try the other USB-C port; use a data cable; install CP210x/CH34x driver; force download mode (Part 5.3). |
| `zugli.local` won't open | mDNS can be flaky on some phones/networks — use the **IP shown on the panel** instead (Part 6.2 / brief §3.3, §7.7). |
| `unknown target triple 'xtensa'` or `libclang` error | Source the `export-esp` script for your shell, then rebuild (Part 4.2 / brief §11). |
| Panel shows `2 Schlieren --` / "no service" | No matching departure on the board right now — normal off-hours; it refreshes next poll (brief §7.7). |

---

## Quick reference — end-to-end

1. Gather parts (Part 1).
2. Wire 14 signal lines + common ground, power off (Part 2).
3. Connect 5 V/5 A PSU to panel, USB to ESP32 (Part 3).
4. Install toolchain once (Part 4).
5. `cd firmware && cargo run` to flash (Part 5).
6. Join `Zügli-Setup` → enter home WiFi → reconnect phone → open `zugli.local` (or the IP
   shown on the panel) → pick stop/line → Save (Part 6).
7. Hold BOOT 3 s to reset WiFi later (Part 7).
