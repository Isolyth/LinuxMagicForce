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

With Nix:

```sh
nix build
```

The binary will be at `result/bin/force-touchd`.

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

Dry-run through the flake:

```sh
nix run . -- --config config/force-touch-linux.toml --dry-run
```

## Systemd

The repository includes a unit at `systemd/force-touchd.service`. It expects:

- binary: `/usr/local/bin/force-touchd`
- config: `/etc/force-touch-linux/config.toml`

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

## NixOS

The flake exposes:

- `packages.${system}.default`: the `force-touchd` package.
- `apps.${system}.default`: runs `force-touchd`.
- `devShells.${system}.default`: Rust development shell.
- `nixosModules.default`: NixOS module that creates `force-touchd.service`.
- `overlays.default`: adds `linux-magic-force` to `pkgs`.

For NixOS, prefer the module instead of manually installing the systemd unit:

```nix
{
  inputs.linuxMagicForce.url = "github:Isolyth/LinuxMagicForce";

  outputs = { nixpkgs, linuxMagicForce, ... }: {
    nixosConfigurations.host = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        linuxMagicForce.nixosModules.default
        {
          services.linuxMagicForce.enable = true;
        }
      ];
    };
  };
}
```

That creates `force-touchd.service` with the default config embedded in the Nix
store. It runs as root, because the daemon needs access to `hidraw` and
`/dev/uinput`.

Use a local config file:

```nix
{
  services.linuxMagicForce = {
    enable = true;
    configFile = ./force-touch-linux.toml;
  };
}
```

Or inline config text:

```nix
{
  services.linuxMagicForce = {
    enable = true;
    configText = builtins.readFile ./force-touch-linux.toml;
  };
}
```

Use the bundled scroll-haptics config from the flake input:

```nix
{
  services.linuxMagicForce = {
    enable = true;
    configText = builtins.readFile "${linuxMagicForce}/config/scroll-haptics.toml";
  };
}
```

Pass extra daemon flags:

```nix
{
  services.linuxMagicForce = {
    enable = true;
    extraArgs = [ "--no-restore" ];
  };
}
```

After rebuilding, use the normal systemd tools:

```sh
sudo systemctl status force-touchd.service
journalctl -u force-touchd.service -f
```

## Configs

```text
config/force-touch-linux.toml  # click/force-click haptics, scroll haptics off
config/scroll-haptics.toml     # click/force-click haptics plus scroll texture
config/usbc-scroll-haptics.toml # USB-C force-click plus softer scroll texture
config/ridge-haptics.toml      # center-ridge haptic test config
```

When no local config is provided, the daemon also checks:

```text
/etc/force-touch-linux/config.toml
```

The daemon rejects unknown config keys. Supported config keys are below.

### Device

```toml
[device]
path = "auto"
restore_on_exit = true
settle_ms = 20.0
```

- `path`: hidraw device path, or `"auto"` to find the Bluetooth Magic Trackpad 2.
- `restore_on_exit`: restores firmware/default click mode when the daemon exits.
- `settle_ms`: delay after enabling host-click mode before reading input.

### Input

```toml
[input]
enabled = true
force_button = "right" # left | right | middle | none
pending_left_ms = 150.0
pending_left_motion = 30.0
multi_touch_drag_motion = true
multi_touch_motion_x_scale = 0.5
multi_touch_motion_y_scale = 0.5
multi_touch_motion_debug = false
```

- `enabled`: enables `/dev/uinput` mouse events. Haptics still run when disabled.
- `force_button`: button emitted by one-finger force click.
- `pending_left_ms`: delays one-finger left-button down so a quick force click can replace it.
- `pending_left_motion`: commits pending left-button down early after this much centroid movement, so drags start promptly.
- `multi_touch_drag_motion`: while a two-finger right click or three-finger middle click is held, emits relative cursor motion from the virtual mouse. Normal two-finger movement without a click is still left to libinput as scroll.
- `multi_touch_motion_x_scale` / `multi_touch_motion_y_scale`: convert raw trackpad centroid deltas to relative mouse motion during multi-touch click drags. Flip the sign if drag direction is inverted.
- `multi_touch_motion_debug`: logs synthesized multi-touch motion deltas.

### Clicks

```toml
[clicks]
normal_threshold = 70
force_threshold = 165
double_click_force_threshold = 220
double_click_window_ms = 350.0
reset_threshold = 35
rearm_threshold = 65
force_rearm_threshold = 125
normal_release_threshold = 60
drag_release_threshold = 30
drag_release_motion = 30.0
min_gap_ms = 120.0
```

- `normal_threshold`: pressure that starts the first click stage.
- `force_threshold`: pressure that starts force click.
- `double_click_force_threshold`: harder force threshold used after a recent click release.
- `double_click_window_ms`: time window for the double-click-drag guard.
- `reset_threshold`: very-low pressure reset threshold for the click state machine.
- `rearm_threshold`: pressure below this re-arms the next normal click after release.
- `force_rearm_threshold`: pressure below this re-arms force click while normal click remains held.
- `normal_release_threshold`: pressure at or below this releases normal non-drag mouse input.
- `drag_release_threshold`: pressure at or below this releases normal mouse input after drag motion is detected.
- `drag_release_motion`: centroid movement that marks a normal click as a drag.
- `min_gap_ms`: minimum gap between normal-click and force-click haptics, except while pending-left is still unresolved.

### Release

```toml
[release]
drop = 18
rate = 700.0
lockout_ms = 25.0
stacked_release_gap_ms = 35.0
suppress_normal_up_after_force_ms = 80.0
```

- `drop`: release haptic fires when pressure falls this far from peak.
- `rate`: release haptic fires when pressure drops this fast per second.
- `lockout_ms`: ignores early drop/rate release detection after a down haptic.
- `stacked_release_gap_ms`: delay between force-up and normal-up haptics when both are sent.
- `suppress_normal_up_after_force_ms`: skips the normal-up haptic if it follows force-up within this window.

### Drag Release

```toml
[drag_release]
enabled = true
movement = 30.0
threshold = 30
debug = false
```

- `enabled`: enables the lower release threshold for click-drags.
- `movement`: centroid movement that marks a normal click as a drag.
- `threshold`: pressure at or below this releases normal mouse input during a drag.
- `debug`: logs when drag release mode activates.

`clicks.drag_release_motion` and `clicks.drag_release_threshold` are accepted aliases used by the Python probe config path; `drag_release.movement` and `drag_release.threshold` are the canonical daemon keys.

### Haptics

The haptics settings only change what the click feels like. The `[clicks]` and
`[release]` sections decide when haptics fire; this section decides which HID
report bytes are sent when they do.

The trackpad haptic command is a 15-byte HID report. The daemon starts from
known working down/up report templates and exposes only the fields we have been
tuning. The byte numbers below are zero-based offsets in the final HID report,
matching the probe/sweep terminology.

Every haptic pulse has two parts:

- a down report, used for the click impact;
- an up report, used for release or for ending short synthetic pulses.

Click and force-click haptics send those reports when the pressure state machine
crosses the configured thresholds. Drag and ridge haptics send a down report,
wait `down_ms`, then send the matching up report.

```toml
[haptics]
down_param = 0x40170606
up_param = 0x26140000

[haptics.normal]
down_byte3 = 0x17
up_byte3 = 0x14

[haptics.force]
down_byte3 = 0x17
up_byte3 = 0x14
```

`down_param` and `up_param` are packed byte fields. For a value written as
`0xAABBCCDD`, the daemon currently uses:

| Field | Report byte | Meaning |
| --- | --- | --- |
| `0xBB` | byte 3 | Main strength-like field. This has been the safest primary tuning knob. |
| `0xCC` | byte 6 | Sharper/louder click character. Strong values can get aggressive quickly. |
| `0xDD` | byte 11 | Deeper/louder click character. Strong values can feel more vertical. |

The leading `0xAA` byte is ignored by the current daemon. With the default
`down_param = 0x40170606`, the base down report gets byte 3 `0x17`, byte 6
`0x06`, and byte 11 `0x06`. With `up_param = 0x26140000`, the base up report
gets byte 3 `0x14`, byte 6 `0x00`, and byte 11 `0x00`.

The `[haptics.normal]` and `[haptics.force]` sections override only byte 3 after
the packed param is applied:

- `haptics.normal.down_byte3`: byte 3 for normal click down haptics.
- `haptics.normal.up_byte3`: byte 3 for normal click up haptics.
- `haptics.force.down_byte3`: byte 3 for force-click down haptics.
- `haptics.force.up_byte3`: byte 3 for force-click up haptics.

For example, to make only force-click impact stronger without changing byte 6 or
byte 11, raise `haptics.force.down_byte3` and leave `down_param` alone.

Drag haptics default to the normal click down/up reports, but can override
`down_param`, `up_param`, `down_byte3`, and `up_byte3` under `[drag_haptics]`.
Ridge haptics have their own haptic fields under `[ridge_haptics]`.

Practical tuning order:

- adjust `down_byte3` first for impact strength;
- adjust `up_byte3` only if release haptics feel too weak or too obvious;
- adjust `down_ms`, `distance`, `min_gap_ms`, and `pulses` for drag/ridge timing;
- touch `down_param` or `up_param` only when intentionally changing byte 6 or
  byte 11.

Empirical tuning notes from this hardware:

- byte 3 behaves like the main strength control and has been usable across the
  full byte range during short tests;
- byte 6 adds a clickier, louder component and should usually stay at or below
  `0x20`;
- byte 11 adds a deeper/louder component and should usually stay at or below
  `0x40`;
- byte 6 and byte 11 can fail to produce a clean click when tested with no
  finger on the pad, so always judge tuning under real touch conditions.

The daemon does not currently clamp packed `*_param` fields. Treat byte 6 and
byte 11 values above those ranges as experimental hardware-stress settings, and
avoid sustained operation while testing them. `--dry-run` prints the final
reports, which is the easiest way to confirm what bytes a config will send.

### Drag Haptics

```toml
[drag_haptics]
enabled = false
mode = "scroll"
distance = 12.0
two_finger_distance = 14.0
three_finger_distance = 24.0
min_gap_ms = 20.0
two_finger_min_gap_ms = 20.0
three_finger_min_gap_ms = 20.0
down_ms = 4.0
motion_epsilon = 2.0
click_silence_ms = 0.0
max_pressure = 120
down_param = 0x40170101
up_param = 0x26140000
down_byte3 = 0x08
up_byte3 = 0x14
debug = false
```

- `enabled`: enables repeated motion haptics.
- `mode`: `"any"` for any touch movement, or `"scroll"` for two-/three-finger scroll movement only.
- `distance`: base centroid travel distance between drag haptics.
- `two_finger_distance`: optional two-finger distance override.
- `three_finger_distance`: optional three-finger distance override.
- `min_gap_ms`: base minimum time between drag haptics.
- `two_finger_min_gap_ms`: optional two-finger minimum-gap override.
- `three_finger_min_gap_ms`: optional three-finger minimum-gap override.
- `down_ms`: delay between drag haptic down/up reports.
- `motion_epsilon`: ignores per-sample movement at or below this distance.
- `click_silence_ms`: suppresses drag haptics briefly after click haptics.
- `max_pressure`: optional pressure cap for drag haptics. Omit to allow any pressure.
- `down_param` / `up_param`: optional packed haptic parameter override for drag haptics.
- `down_byte3` / `up_byte3`: optional byte 3 override for drag haptics.
- `debug`: logs drag haptic trigger decisions.

### Ridge Haptics

```toml
[ridge_haptics]
enabled = false
position = 0.0
grid_columns = 1
grid_rows = 1
grid_x_min = -3600.0
grid_x_max = 3600.0
grid_y_min = -2400.0
grid_y_max = 2400.0
deadband = 80.0
min_gap_ms = 80.0
down_ms = 4.0
pulses = 1
pulse_gap_ms = 10.0
down_param = 0x40170606
up_param = 0x26140000
down_byte3 = 0x17
up_byte3 = 0x14
debug = false
```

- `enabled`: enables ridge-crossing haptics.
- `position`: center x-position for the single vertical ridge.
- `grid_columns`: number of vertical ridge columns.
- `grid_rows`: number of horizontal ridge rows.
- `grid_x_min` / `grid_x_max`: x-coordinate bounds for ridge grid placement.
- `grid_y_min` / `grid_y_max`: y-coordinate bounds for ridge grid placement.
- `deadband`: distance around a ridge before the crossing can trigger again.
- `min_gap_ms`: minimum gap between ridge haptics.
- `down_ms`: delay between ridge haptic down/up reports.
- `pulses`: number of pulses per ridge crossing.
- `pulse_gap_ms`: delay between pulses.
- `down_param`: base packed haptic parameter for ridge down reports.
- `up_param`: base packed haptic parameter for ridge up reports.
- `down_byte3`: byte 3 override for ridge down haptic strength.
- `up_byte3`: byte 3 override for ridge up haptic strength.
- `debug`: logs ridge crossing decisions.

## CLI Overrides

The force-click action and the main click tuning values can also be overridden
from the command line:

```sh
cargo run -- --config config/force-touch-linux.toml --force-button middle --dry-run
cargo run -- --config config/force-touch-linux.toml --pending-left-ms 0 --dry-run
cargo run -- --config config/force-touch-linux.toml --normal-release-threshold 55 --drag-release-threshold 25 --dry-run
```
