# Zügli

A DIY, ESP32-driven transit departure board for Swiss public transport. You configure
**one stop and one connection** from your phone, and the device shows a live countdown to
the next departure of that connection on a 64×64 LED matrix panel. It sits in your home,
so you can see from across the room exactly when to leave.

**https://aarkro.github.io/zugli/**

## Documentation

- **[PROJECT_BRIEF.md](PROJECT_BRIEF.md)** — the full implementation brief: architecture,
  the three runtime phases, the config web page, the firmware crate stack, the transport
  API reference, and all settled decisions.
- **[ASSEMBLY_AND_FLASHING.md](ASSEMBLY_AND_FLASHING.md)** — the practical "how to build
  one" guide: bill of materials, wiring the panel, installing the toolchain, flashing the
  firmware, and first-time setup.