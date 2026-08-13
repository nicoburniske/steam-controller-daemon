# Steam Controller Daemon (scd)

an experimental Linux-native userspace daemon for the Steam Controller (2026)

scd is built for power users who want a config file, not an opaque Steam Input GUI

expect rough edges and breaking changes

![scd on-screen keyboard](scd-demo.gif)

## features

- declarative KDL configuration
  - validation
  - live reload
- input mapping
  - keyboard and mouse output
  - Xbox + wired DualShock 4 gamepad emulation
  - gyro and trackpad controls
- modes and hold layers
- global button chords
- haptic feedback
- themable Wayland native on-screen keyboard
- NixOS module

## usb architecture

scd talks to the physical Steam Controller receiver directly through Linux
`usbfs` ioctls, claims the receiver's four controller interfaces, and keeps two
interrupt reads queued for each one so the kernel can continue receiving
reports while scd maps input or sends haptics, with haptic writes also queued
rather than blocking input processing

this replaces HIDAPI rather than wrapping it because HIDAPI made access depend
on a set of `hidraw` nodes and their permissions, obscured the receiver's
multi-interface lifecycle, and did not provide the queued transfers needed for
reliable low-latency input, while the internal `usbfs` module implements only
the Linux operations scd uses

the emulated wired DualShock 4 needs complete USB topology, including its
descriptors, control endpoint, feature reports, and interrupt endpoints, so scd
implements a fixed local USB/IP device backend and attaches its socket directly
to `vhci_hcd`, giving Linux, Wine, and games the wired USB interface they expect
