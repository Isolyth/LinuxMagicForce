# LinuxMagicForce

Userspace Magic Trackpad 2 Force Touch and haptics daemon for Linux.

This is the Rust daemon port of the ForceTouchLinux probe. It talks to the
Bluetooth Magic Trackpad 2 through `hidraw`, enables host-click mode, sends
haptic output reports, and emits mouse buttons through `/dev/uinput`.

## Current Defaults

The checked-in config matches the tuned Python command:

```sh
sudo python3 tools/mt2_haptic_probe.py pressure-clicks --drag-haptics --drag-haptic-mode scroll --drag-two-finger-distance 14 --drag-three-finger-distance 24 --drag-interval-ms 20
```

That means:

- normal click pressure threshold: `70`
- force click pressure threshold: `165`
- force click emits right click
- two-finger normal click emits right click
- three-or-more-finger normal click emits middle click
- drag haptics are enabled only for two- or three-finger scroll movement
- two-finger drag haptic distance: `14`
- three-finger drag haptic distance: `24`
- drag haptic minimum gap: `20ms`

## Build

```sh
cargo build
```

Dry-run the resolved config and report bytes:

```sh
cargo run -- --config config/force-touch-linux.toml --dry-run
```

Run against the trackpad:

```sh
sudo target/debug/force-touchd --config config/force-touch-linux.toml
```

Stop with `Ctrl-C`; the daemon restores firmware/default click mode on exit
unless `--no-restore` or `restore_on_exit = false` is set.

## Config

The default config lives at:

```text
config/force-touch-linux.toml
```

Key tuning knobs:

```toml
[drag_haptics]
enabled = true
mode = "scroll"
two_finger_distance = 14.0
three_finger_distance = 24.0
min_gap_ms = 20.0
motion_epsilon = 2.0

[clicks]
normal_threshold = 70
force_threshold = 165
```

The daemon also looks for `/etc/force-touch-linux/config.toml` when no local
config file is present.
