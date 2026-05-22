# LinuxMagicForce

LinuxMagicForce is a userspace daemon for Bluetooth Magic Trackpad 2 haptics on
Linux. It reads the trackpad through `hidraw`, enables host-click mode, sends
haptic reports, and emits mouse button events through `/dev/uinput`.

## Behavior

- One-finger click emits left click.
- Two-finger click emits right click.
- Three-or-more-finger click emits middle click.
- Force click emits a configurable button action.
- Optional scroll haptics can add texture during two- or three-finger scrolling.

The daemon restores firmware/default click mode when it exits unless
`--no-restore` or `restore_on_exit = false` is set.

## Build

```sh
cargo build
```

## Run

Run the daemon:

```sh
sudo target/debug/force-touchd --config config/force-touch-linux.toml
```

Run with scroll haptics enabled:

```sh
sudo target/debug/force-touchd --config config/scroll-haptics.toml
```

Stop with `Ctrl-C`.

## Configs

```text
config/force-touch-linux.toml  # click/force-click haptics, scroll haptics off
config/scroll-haptics.toml     # click/force-click haptics plus scroll texture
```

Important settings:

```toml
[input]
enabled = true
force_button = "right" # left | right | middle | none

[clicks]
normal_threshold = 70
force_threshold = 165

[drag_haptics]
enabled = false
mode = "scroll" # any | scroll
two_finger_distance = 14.0
three_finger_distance = 24.0
min_gap_ms = 20.0
motion_epsilon = 2.0
```

When no local config is provided, the daemon also checks:

```text
/etc/force-touch-linux/config.toml
```
