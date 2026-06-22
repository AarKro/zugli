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

## Documentation

- **[PROJECT_BRIEF.md](PROJECT_BRIEF.md)** — the full implementation brief: architecture,
  the three runtime phases, the config web page, the firmware crate stack, the transport
  API reference, and all settled decisions.
- **[ASSEMBLY_AND_FLASHING.md](ASSEMBLY_AND_FLASHING.md)** — the practical "how to build
  one" guide: bill of materials, wiring the panel, installing the toolchain, flashing the
  firmware, and first-time setup.