# Zügli

A DIY, ESP32-driven transit departure board for Swiss public transport. You configure
**one stop and one connection** from your phone, and the device shows a live countdown to
the next departure of that connection on a 64×64 LED matrix panel. It sits in your home,
so you can see from across the room exactly when to leave.

**https://aarkro.github.io/zugli/**

## How it works

- The device runs on an **ESP32-S3** driving a single native **64×64 HUB75** RGB LED panel.
- On first power-up it has no WiFi, so it opens a **`Zügli-Setup`** hotspot with a captive
  portal where you pick your home network.
- Once online, it serves a small config page at **`zugli.local`** where you search for a
  stop and pick the connection you ride.
- From then on the firmware polls the Swiss transport API every 30 seconds and renders the
  countdown to your next departures on the panel.

## Repository layout

```
zugli/
├─ PROJECT_BRIEF.md          # full implementation brief
├─ ASSEMBLY_AND_FLASHING.md  # build-one + flash guide
├─ designs/                  # design assets (mockups, use-case diagrams, setup page)
├─ web/                      # pages served by the device
│   ├─ index.html            #   Phase 2 config page
│   └─ setup.html            #   Phase 1 captive-portal page
├─ site/                     # the public landing page (GitHub Pages)
└─ firmware/                 # the ESP32-S3 no_std Rust firmware
    └─ src/
        ├─ bin/main.rs       #   boot + orchestration
        ├─ display.rs        #   HUB75 dual-core render (placeholder layout)
        ├─ wifi.rs           #   STA / SoftAP + embassy-net
        ├─ portal.rs         #   Phase 1: DHCP + DNS catch-all + setup server
        ├─ httpd.rs          #   Phase 2: config server + POST /save
        ├─ poll.rs           #   Phase 3: HTTPS poll + JSON + minutes
        ├─ sntp.rs           #   time sync
        ├─ storage.rs        #   WiFi creds + selection in the nvs partition
        ├─ model.rs / shared.rs
```

The `web/` pages are embedded into the firmware binary at build time (`include_str!`), so
they ship with the flash image — there is no separate upload step.

## Building & flashing the firmware

You need the Xtensa Rust toolchain (see **[ASSEMBLY_AND_FLASHING.md](ASSEMBLY_AND_FLASHING.md)**
Part 4 for one-time setup: `espup`, `espflash`, the `esp` toolchain).

```bash
cd firmware
cargo build        # compile-check — works with no board attached
cargo run          # build + flash + serial monitor — needs the ESP32-S3 over USB
```

> **Linker on PATH.** The final link step needs the Xtensa GCC linker
> (`xtensa-esp32s3-elf-gcc`). If `cargo build` fails with *"linker … not found"*, source
> the espup environment first so it's on PATH:
> ```bash
> . ~/export-esp.sh        # bash/zsh   (fish users: translate the PATH line, see brief §11)
> ```

## First-time setup (UC1 → UC2)

1. Power on with no saved WiFi → the device opens the open **`Zügli-Setup`** hotspot.
2. Join it from your phone; the captive portal opens (or browse to `http://192.168.4.1`).
3. Pick your home network, enter the password, **Connect**. The device saves it and reboots.
4. Reconnect your phone to your home WiFi and open **`http://zugli.local`** (if that doesn't
   resolve, the panel shows the device IP — type that instead).
5. Search a stop, pick a connection, **Save to Zügli**. The panel updates within one poll.

**Reset WiFi (UC3):** hold the BOOT button for 3 s. This clears the WiFi credentials only
(your saved stop/connection is kept) and reboots into the captive portal.

## Implementation notes & known limitations

The firmware **compiles and links** for `xtensa-esp32s3-none-elf`. It has not yet been run
on physical hardware, so the items below are flagged for on-device verification:

- **RAM / PSRAM.** The large TLS record and HTTP response buffers live in the board's 8 MB
  **PSRAM** (`esp_alloc::psram_allocator!`), while WiFi's DMA-capable allocations use a
  reclaimed internal-SRAM heap. PSRAM auto-detection for the N16R8's *octal* SPIRAM should
  be confirmed on hardware; size/mode may need tuning in `main.rs`.
- **WiFi + HUB75 flicker** (brief §7.6) is mitigated structurally — the render loop is
  pinned to the second core and `esp-hub75`'s `iram` feature is on — but the pixel clock
  and the optional 74HCT245 level shifter may still need tuning on real hardware.
- **mDNS (`zugli.local`) is not yet implemented.** The device advertises its address via
  the **IP shown on the LED panel** (the idle screen, brief §7.7), which is the documented
  fallback. Adding an `edge-mdns` responder is a clean follow-up.
- **Captive-portal DHCP/DNS** use the `edge-dhcp` packet codec and a tiny custom DNS
  responder driven over embassy-net UDP sockets; the DHCP reply is broadcast to `:68`. This
  path should be exercised against a real phone.
- **Dependency pins.** `reqwless 0.14` pulls `embedded-tls 0.18`, whose `rustpki` module
  only compiles with `der`'s `heapless` feature enabled — forced on via a direct `der`
  dependency in `Cargo.toml`. `portable-atomic` provides the 64-bit atomics the Xtensa core
  lacks. See the comments in `firmware/Cargo.toml`.
- **TLS signature schemes.** reqwless is built with its `alloc` feature on so embedded-tls
  advertises the `rsa_pss_rsae_*` signature schemes. The poll target serves an **RSA**
  certificate, and TLS 1.3 requires the server's CertificateVerify to be RSA-PSS-signed;
  without those schemes advertised the handshake aborts with `HandshakeFailure`. (The heavy
  `rsa` crate is not pulled in — `TlsVerify::None` skips verifying the signature, so merely
  advertising the schemes is enough for the server to complete the handshake.)

## Security note

The firmware's outbound HTTPS poll uses **`embedded-tls` with certificate verification
disabled** (`TlsVerify::None`, brief decision §8-4). `no_std` certificate verification is
not wired up, which is an accepted trade-off for a home device on a trusted network. The
served pages are plain HTTP on the LAN. Hardening to verified TLS (e.g. `esp-mbedtls`, which
would also bring full certificate validation) is a possible future step. Do not treat this
device as exposed to untrusted networks.

## Documentation

- **[PROJECT_BRIEF.md](PROJECT_BRIEF.md)** — the full implementation brief: architecture,
  the three runtime phases, the config web page, the firmware crate stack, the transport
  API reference, and all settled decisions.
- **[ASSEMBLY_AND_FLASHING.md](ASSEMBLY_AND_FLASHING.md)** — the practical "how to build
  one" guide: bill of materials, wiring the panel, installing the toolchain, flashing the
  firmware, and first-time setup.