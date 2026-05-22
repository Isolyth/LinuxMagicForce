# LinuxMagicForce

LinuxMagicForce is a userspace daemon for Bluetooth Magic Trackpad 2 haptics on
Linux. It reads the trackpad through `hidraw`, enables host-click mode, sends
haptic reports, and emits mouse button events through `/dev/uinput`.

## Behavior

- One-finger click emits left click.
- Two-finger click emits right click.
- Three-or-more-finger click emits middle click.
- Force click emits a configurable button action.
- A second one-finger click inside the double-click window uses a harder force
  threshold while held, which makes double-click-drag easier to perform without
  accidentally triggering force click.
- One-finger click-drag can require a lower pressure before releasing the left
  button, which makes held drags less likely to release accidentally.
- Optional scroll haptics can add texture during two- or three-finger scrolling.

The daemon restores firmware/default click mode when it exits unless
`--no-restore` or `restore_on_exit = false` is set.

## Build

```sh
cargo build
```

For installable binaries:

```sh
cargo build --release
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

## Systemd

Install the release binary, config, and service unit:

```sh
sudo install -Dm755 target/release/force-touchd /usr/local/bin/force-touchd
sudo install -Dm644 config/force-touch-linux.toml /etc/force-touch-linux/config.toml
sudo install -Dm644 systemd/force-touchd.service /etc/systemd/system/force-touchd.service
sudo systemctl daemon-reload
sudo systemctl enable --now force-touchd.service
```

Use `config/scroll-haptics.toml` instead when installing the config if scroll
haptics should be enabled by default.

Watch logs:

```sh
journalctl -u force-touchd.service -f
```

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
double_click_force_threshold = 220
double_click_window_ms = 350.0

[drag_release]
enabled = true
movement = 80.0
threshold = 45

[drag_haptics]
enabled = false
mode = "scroll" # any | scroll
two_finger_distance = 14.0
three_finger_distance = 24.0
min_gap_ms = 20.0
motion_epsilon = 2.0
```

The force-click action can also be overridden from the command line:

```sh
cargo run -- --config config/force-touch-linux.toml --force-button middle --dry-run
```

When no local config is provided, the daemon also checks:

```text
/etc/force-touch-linux/config.toml
```
