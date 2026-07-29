# Steam Controller Daemon (scd)

an experimental Linux-native userspace daemon for the Steam Controller (2026)

scd is built for power users who want a config file, not an opaque Steam Input GUI

expect rough edges and breaking changes

![scd on-screen keyboard](scd-demo.gif)

## features

- declarative TOML configuration
  - validation
  - live reload
- input mapping
  - keyboard and mouse output
  - Xbox + DualShock 4 gamepad emulation
  - gyro and trackpad controls
- modes and hold layers
- global button chords
- haptic feedback
- runtime Steam Input handoff
- themable Wayland native on-screen keyboard
- NixOS module
