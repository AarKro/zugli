# Zügli — Assembly & Software Loading Guide

This guide walks you through building the Zügli transit board and loading its
software, one step at a time. You don't need to be an electronics or programming
expert — if you can follow a recipe and plug in labelled wires, you can build one.

Here's the whole journey, start to finish:

**Gather the parts → connect the wires → power it up → install the software tools →
load the software onto the board → set it up from your phone.**

> This guide is the practical "how to build one" companion to
> [`PROJECT_BRIEF.md`](PROJECT_BRIEF.md). When a detail here (a wire, a setting, a
> decision) needs a deeper explanation, the brief is cited like **(brief §3.1)**.

---

## Quick overview — the whole build at a glance

If you just want the big picture before diving in, here are the seven steps. Each
one is explained in full further down.

1. **Gather the parts** (Part 1).
2. **Connect the 14 signal wires plus a shared ground wire** — always with the power
   off (Part 2).
3. **Plug in the power:** the 5 V / 5 A supply into the panel, and the USB cable into
   the small computer board (Part 3).
4. **Install the software tools** on your computer — you only do this once (Part 4).
5. **Load the software onto the board** by running `cargo run` (Part 5).
6. **Set it up from your phone:** join the `Zügli-Setup` WiFi → enter your home WiFi
   → reconnect your phone to your home WiFi → open `zugli.local` (or the address
   shown on the panel) → choose what to track → pick your stop → Save (Part 6).
7. **To start over later,** hold the BOOT button for 3 seconds to factory-reset the
   device (Part 7).

---

## Part 0 — Before you start

**You don't need the board to write or check the software.** The `cargo build`
command works on your computer alone (brief §11). You only need the physical board
for the loading and on-device steps (Parts 2–6). If you only want to check that the
code compiles, skip ahead to Part 5.1.

**A few safety rules — please read these once:**

- **Always connect or change wires with the power disconnected.** Only turn on the
  5 V power after you've double-checked every connection.
- **The grounds must be tied together.** The small computer board (the ESP32) and the
  panel/power-supply must share a common ground wire, or the panel will show garbage
  or nothing at all (brief §3). More on this in Part 2.2.
- **Never power the LED panel from the board's USB port.** A 64×64 panel can draw
  several amps of current — far more than a USB port can give. Always use the
  dedicated 5 V / 5 A power supply (brief §3).
- **Watch the power-supply polarity.** Swapping the 5 V and ground wires into the
  panel can permanently destroy it.

---

## Part 1 — What you need (gather these first)

| # | Part | What it is / what to look for |
|---|------|-------------------------------|
| 1 | **ESP32-S3-DevKitC** (the "board") | The small WiFi computer board that runs everything. Get the **N16R8 variant** (16 MB flash, 8 MB PSRAM). It has a small **BOOT** button you'll use later. (brief §3) |
| 2 | **64×64 HUB75E RGB LED panel** | The display itself. It must be a single **native** 64×64 panel with **P3** spacing, **1/32-scan**, and address lines labelled **A through E** (the "E" is important). It must *not* be two smaller 64×32 panels joined together. (brief §3, §3.2) |
| 3 | **5 V / 5 A power supply** | Powers the panel. The 5-amp rating gives comfortable headroom for a bright display. (brief §3) |
| 4 | **HUB75 power lead** | The short 2-pin/4-pin power cable (spade or screw-terminal) that comes with the panel. It carries power from the supply to the panel's `+5V`/`GND` terminals. (brief §3) |
| 5 | **Jumper wires** | About 16 female-to-female "Dupont" wires — enough for the 14 signal lines plus ground. Shorter wires work more reliably. (brief §3.1) |
| 6 | **USB-C cable** | A **data-capable** cable (not a charge-only one) to connect the board to your computer. (brief §11) |

> The panel's input is a small 16-pin connector (a 2×8 header). Panels usually have an
> **IN** side and an **OUT** side — always connect to **IN** (look for an arrow, "J1",
> or "IN" printed on the panel).

---

## Part 2 — Connect the panel to the board

Below are the **exact wire connections the software expects** (brief §3.1). The
simplest path is to wire it exactly as shown — if you connect a wire differently,
you'd have to change a matching setting in the software.

Each connection runs from a numbered pin on the board (a "GPIO") to a labelled pad on
the panel's connector.

### 2.1 The 14 signal wires (board GPIO → panel IN connector)

| Panel pad | What it does | Board pin |
|-----------|--------------|-----------|
| R1 | top-half red | **GPIO38** |
| G1 | top-half green | **GPIO42** |
| B1 | top-half blue | **GPIO48** |
| R2 | bottom-half red | **GPIO47** |
| G2 | bottom-half green | **GPIO2** |
| B2 | bottom-half blue | **GPIO21** |
| A | address bit 0 | **GPIO14** |
| B | address bit 1 | **GPIO46** |
| C | address bit 2 | **GPIO13** |
| D | address bit 3 | **GPIO9** |
| **E** | address bit 4 | **GPIO3** |
| CLK | pixel clock | **GPIO12** |
| LAT (STB) | latch | **GPIO10** |
| OE (BLANK) | output enable | **GPIO11** |
| GND | ground | **any GND pin on the board** |

> **Don't skip the E wire (GPIO3).** A native 64×64 panel needs all five address
> lines, A through E. Leaving out E is the classic mistake that makes "only half the
> panel" or the wrong rows light up (brief §3.1). Make sure GPIO3 reaches the panel's
> **E** pad.

Each pad on the panel's connector is labelled (R1 G1 / R2 G2 / B1 B2 / A B / C D /
CLK LAT / OE GND / E …). **Match by the printed label, not by physical position** —
the exact layout varies slightly between panel makers. On some panels the E pad sits
where an older panel design had a second ground pad.

### 2.2 The shared ground wire (do not skip this)

Connect **at least one GND pad on the panel's connector to a GND pin on the board.**
This is the shared "signal ground," and it's separate from the heavy power ground in
Part 3. In the end both grounds are connected together — which is exactly what you
want. **A shared common ground is required** (brief §3).

---

## Part 3 — Power it up and do a quick test

1. With **everything switched off**, connect the **5 V / 5 A power supply** to the
   panel's power terminals (`+5V` and `GND`), double-checking you have them the right
   way round (brief §3).
2. Connect the **USB-C cable** from the board to your computer. (This only powers the
   board itself — it does not power the panel.)
3. Re-check: the signal wires match Part 2.1, the **E** wire is connected, and a
   ground pad on the panel connects to a ground pin on the board (Part 2.2).
4. Switch on the power supply. Right now, with no software loaded yet, the panel might
   show random pixels or stay dark — that's completely normal. Nothing should get
   warm. **If anything heats up or smells, cut the power immediately** and re-check
   your polarity and grounds.

The panel will only show real content after you load the software (Part 5) and set it
up (Part 6).

---

## Part 4 — Install the software tools (one time only)

The board uses a special processor design (called **Xtensa**), so it needs a special
version of the Rust programming toolchain rather than the standard one. Because of how
this project is built, you don't need the large ESP-IDF C toolkit — a tool called
`espup` provides everything (brief §11).

Follow the section for your computer.

### 4.1 macOS / Linux

```bash
# macOS only: install Apple's command-line tools (git + a C compiler)
xcode-select --install

# Install Rust via rustup — do NOT use a package manager, it breaks the espup step
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install the special toolchain for the board
cargo install espup --locked
espup install

# Install the loader/monitor and the project scaffolding tool
cargo install espflash --locked
cargo install esp-generate
```

### 4.2 Windows (PowerShell)

```powershell
# Install Rust via rustup (or download it from https://rustup.rs if winget is missing)
winget install Rustlang.Rustup

# Install the special toolchain for the board
cargo install espup --locked
espup install

# Install the loader/monitor and the project scaffolding tool
cargo install espflash --locked
cargo install esp-generate
```

On Windows, `espup install` creates a file at `%USERPROFILE%\export-esp.ps1`. You
usually **don't need to run it** — the project already selects the right toolchain
automatically (brief §11). Only if you later hit an error that mentions `libclang` or
`unknown target triple 'xtensa'` should you run that script in your terminal first.

### 4.3 Check the tools installed correctly

```bash
rustup toolchain list      # the list should include "esp"
espflash --version         # this should print a version number
```

If your terminal says `cargo` isn't found in a new window, add `~/.cargo/bin`
(Windows: `%USERPROFILE%\.cargo\bin`) to your PATH.

---

## Part 5 — Load the software onto the board

> The software lives in the `firmware/` folder of this project — that's what gets
> loaded onto the board. It's already set up for you; the full list of components and
> versions is in `firmware/Cargo.toml` and documented in brief §7.1.

### 5.1 Just check it compiles (no board needed)

Inside the `firmware` folder:

```bash
cd firmware
cargo build          # or: cargo clippy
```

This compiles the software **without the board attached** — the fastest way to
confirm the code is sound (brief §11).

### 5.2 Load it and watch the log (board required)

1. Connect the board to your computer over **USB-C** (the panel stays on its own power
   supply).
2. From the `firmware` folder, run:

   ```bash
   cargo run
   ```

   This **builds the software, loads it onto the board, and opens a live log** — all
   in one command. It needs the board connected over USB (brief §11).

3. Watch the log for startup messages. On a brand-new device with no saved WiFi, the
   software starts **Phase 1** — the WiFi setup mode (the next part).

### 5.3 If the computer doesn't see the board

- The board has **two USB-C ports** — try the **other one** (only one of them is the
  right port for loading) (brief §11).
- Make sure you're using a **data-capable** cable, not a charge-only one.
- Install the USB driver if needed: Silicon Labs **CP210x** or **CH34x** (brief §11).
- As a last resort, force the board into loading mode: hold the **BOOT** button, tap
  **RESET**, release **BOOT**, then run `cargo run` again.

### 5.4 The setup page comes along for the ride

The phone setup page is **built into the software** and served directly by the board.
There's no separate file to upload — when you load the firmware, the setup pages come
with it automatically.

---

## Part 6 — Set it up from your phone (first time)

Once the software is loaded, everything from here is done on your phone — no computer
or code needed.

### 6.1 Connect it to your WiFi (Phase 1)

1. On first power-up with no saved WiFi, the device creates its own **open** WiFi
   hotspot named **`Zügli-Setup`** (no password) (brief §5.1).
2. On your phone, join `Zügli-Setup`. A setup page should pop up automatically. If it
   doesn't, open your browser and go to **`http://192.168.4.1`** (brief §5.1).
3. The page lists the WiFi networks nearby. **Pick your home network, type its
   password, and tap Connect.**
4. The device tries to join:
   - **Wrong password →** it shows an error and sends you back to try again
     (brief §5.1).
   - **Success →** it saves your WiFi and **restarts** into its normal mode.

### 6.2 Find the device on your network

1. After it restarts, **reconnect your phone to your normal home WiFi** — the device
   is no longer broadcasting `Zügli-Setup`, so you switch your phone back manually
   (brief §5.1).
2. In your phone's browser, open **`http://zugli.local`** (brief §3.3).
3. **If `zugli.local` doesn't open** (this can be unreliable on some phones), look at
   the **LED panel** — until a stop is chosen, the panel shows the device's current
   **network address** (for example `192.168.1.42`). Type **`http://`** followed by
   that address into your browser instead (brief §3.3, §7.7).

### 6.3 Choose your stop and connections (Phase 2)

On the setup page (brief §4.2):

1. **Choose what to track** — a switch at the top lets you pick **Specific
   connections** (choose the exact lines you care about) or **All connections**
   (everything at the stop).
2. **Search for your stop** — start typing and a list of matching Swiss stops appears.
   Tap the one you want.
3. **Pick your connections** — in *Specific connections* mode, a list of live
   departures (line number → destination) appears; tap each one you want to track (up
   to 6). In *All connections* mode this step is skipped.
4. Tap **"Save to Zügli."** The device stores your choice and **switches over
   instantly, with no restart** — the panel starts showing departures within about 30
   seconds (brief §4.4).

Tap the **gear icon** any time to open settings: hide city-name prefixes, adjust
brightness, and dim the panel automatically at night (brief §4.6). The setup page
**stays available** at `zugli.local` the whole time the device is running, so you can
change your stop, connections, or settings whenever you like — nothing needs resetting
(brief §2, §4.4).

---

## Part 7 — Start over (factory reset)

To wipe the device back to its out-of-the-box state, **hold the BOOT button for 3 or
more seconds.** This clears **both** the saved WiFi **and** your saved stop/connection
choice (settings return to their defaults), then restarts into the `Zügli-Setup` WiFi
from Part 6.1. After you reconnect to your WiFi, you'll pick your stop and connections
again (Part 6.3) — nothing from before is kept (brief §7.9, §8-5).

---

## Part 8 — Troubleshooting

| What you see | Likely cause / what to do |
|--------------|---------------------------|
| Panel is dark or shows random pixels after loading | Check the signal wiring (Part 2.1) and that a panel **GND** connects to a board **GND** (Part 2.2). |
| Only half the panel, or the wrong rows, light up | The **E** wire (GPIO3) isn't connected — a 64×64 panel needs all of A–E (brief §3.1). |
| Flicker or colour glitches **during** WiFi updates | A timing issue — this is a software setting; see brief §7.6. |
| Computer won't detect the board when loading | Try the other USB-C port; use a data cable; install the CP210x/CH34x driver; force loading mode (Part 5.3). |
| `zugli.local` won't open | This can be unreliable on some phones — use the **address shown on the panel** instead (Part 6.2 / brief §3.3, §7.7). |
| `unknown target triple 'xtensa'` or `libclang` error | Run the `export-esp` script for your terminal, then try again (Part 4.2 / brief §11). |
| Panel shows the stop name and "no service" | Nothing you track is leaving right now — normal during quiet hours; it refreshes on the next update (brief §7.7). |
