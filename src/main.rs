use std::env;
use std::ffi::c_int;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::mem::size_of;
use std::os::fd::{AsRawFd, RawFd};
use std::os::raw::c_ulong;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const BUS_BLUETOOTH: u32 = 0x05;
const APPLE_BT_VENDOR: u16 = 0x004c;
const MAGIC_TRACKPAD2_PRODUCT: u16 = 0x0265;

const HOST_CLICK_ON: [u8; 3] = [0xf2, 0x21, 0x01];
const HOST_CLICK_OFF: [u8; 3] = [0xf2, 0x21, 0x00];

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const SYN_REPORT: u16 = 0x00;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BUS_VIRTUAL: u16 = 0x06;

const MEDIUM_DOWN_PARAM: u32 = 0x40170606;
const MEDIUM_UP_PARAM: u32 = 0x26140000;

const VIB_DOWN_TEMPLATE: [u8; 15] = [
    0xf2, 0x53, 0x01, 0x17, 0x78, 0x02, 0x06, 0x24, 0x30, 0x06, 0x01, 0x06, 0x18, 0x48, 0x12,
];
const VIB_UP_TEMPLATE: [u8; 15] = [
    0xf2, 0x53, 0x01, 0x14, 0x78, 0x02, 0x00, 0x24, 0x30, 0x06, 0x01, 0x00, 0x18, 0x48, 0x12,
];

const LOCAL_CONFIG_PATH: &str = "config/force-touch-linux.toml";
const SYSTEM_CONFIG_PATH: &str = "/etc/force-touch-linux/config.toml";

const IOC_NRBITS: u64 = 8;
const IOC_TYPEBITS: u64 = 8;
const IOC_SIZEBITS: u64 = 14;
const IOC_NRSHIFT: u64 = 0;
const IOC_TYPESHIFT: u64 = IOC_NRSHIFT + IOC_NRBITS;
const IOC_SIZESHIFT: u64 = IOC_TYPESHIFT + IOC_TYPEBITS;
const IOC_DIRSHIFT: u64 = IOC_SIZESHIFT + IOC_SIZEBITS;
const IOC_WRITE: u64 = 1;
const IOC_READ: u64 = 2;
const IOC_NONE: u64 = 0;

const SIGINT: c_int = 2;
const SIGTERM: c_int = 15;
const POLLIN: i16 = 0x0001;

static RUNNING: AtomicBool = AtomicBool::new(true);

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}

unsafe extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    fn signal(signum: c_int, handler: extern "C" fn(c_int)) -> usize;
    fn poll(fds: *mut PollFd, nfds: c_ulong, timeout: c_int) -> c_int;
}

extern "C" fn handle_signal(_: c_int) {
    RUNNING.store(false, Ordering::SeqCst);
}

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DragMode {
    Any,
    Scroll,
}

impl DragMode {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "any" => Ok(Self::Any),
            "scroll" => Ok(Self::Scroll),
            _ => Err("drag_haptics.mode must be 'any' or 'scroll'".to_string()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Scroll => "scroll",
        }
    }
}

#[derive(Clone, Debug)]
struct DeviceConfig {
    path: Option<String>,
    restore_on_exit: bool,
    settle_ms: f64,
}

#[derive(Clone, Debug)]
struct InputConfig {
    enabled: bool,
    force_button: Option<u16>,
    pending_left_ms: f64,
    pending_left_motion: f64,
    multi_touch_drag_motion: bool,
    multi_touch_motion_x_scale: f64,
    multi_touch_motion_y_scale: f64,
    multi_touch_motion_debug: bool,
}

#[derive(Clone, Debug)]
struct ClickConfig {
    normal_threshold: i32,
    force_threshold: i32,
    double_click_force_threshold: i32,
    double_click_window_ms: f64,
    reset_threshold: i32,
    rearm_threshold: i32,
    force_rearm_threshold: i32,
    normal_release_threshold: i32,
    min_gap_ms: f64,
}

#[derive(Clone, Debug)]
struct ReleaseConfig {
    drop: i32,
    rate: f64,
    lockout_ms: f64,
    stacked_release_gap_ms: f64,
    suppress_normal_up_after_force_ms: f64,
}

#[derive(Clone, Debug)]
struct DragReleaseConfig {
    enabled: bool,
    movement: f64,
    threshold: i32,
    debug: bool,
}

#[derive(Clone, Debug)]
struct HapticsConfig {
    down_param: u32,
    up_param: u32,
    normal_down_byte3: u8,
    normal_up_byte3: u8,
    force_down_byte3: u8,
    force_up_byte3: u8,
}

#[derive(Clone, Debug)]
struct DragConfig {
    enabled: bool,
    mode: DragMode,
    distance: f64,
    two_finger_distance: Option<f64>,
    three_finger_distance: Option<f64>,
    min_gap_ms: f64,
    two_finger_min_gap_ms: Option<f64>,
    three_finger_min_gap_ms: Option<f64>,
    down_ms: f64,
    motion_epsilon: f64,
    click_silence_ms: f64,
    max_pressure: Option<i32>,
    debug: bool,
}

#[derive(Clone, Debug)]
struct RidgeConfig {
    enabled: bool,
    position: f64,
    grid_columns: i32,
    grid_rows: i32,
    grid_x_min: f64,
    grid_x_max: f64,
    grid_y_min: f64,
    grid_y_max: f64,
    deadband: f64,
    min_gap_ms: f64,
    down_ms: f64,
    pulses: i32,
    pulse_gap_ms: f64,
    down_param: u32,
    up_param: u32,
    down_byte3: u8,
    up_byte3: u8,
    debug: bool,
}

#[derive(Clone, Debug)]
struct Config {
    device: DeviceConfig,
    input: InputConfig,
    clicks: ClickConfig,
    release: ReleaseConfig,
    drag_release: DragReleaseConfig,
    haptics: HapticsConfig,
    drag: DragConfig,
    ridge: RidgeConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device: DeviceConfig {
                path: None,
                restore_on_exit: true,
                settle_ms: 20.0,
            },
            input: InputConfig {
                enabled: true,
                force_button: Some(BTN_RIGHT),
                pending_left_ms: 150.0,
                pending_left_motion: 30.0,
                multi_touch_drag_motion: true,
                multi_touch_motion_x_scale: 1.0,
                multi_touch_motion_y_scale: 1.0,
                multi_touch_motion_debug: false,
            },
            clicks: ClickConfig {
                normal_threshold: 70,
                force_threshold: 165,
                double_click_force_threshold: 220,
                double_click_window_ms: 350.0,
                reset_threshold: 35,
                rearm_threshold: 65,
                force_rearm_threshold: 125,
                normal_release_threshold: 60,
                min_gap_ms: 120.0,
            },
            release: ReleaseConfig {
                drop: 18,
                rate: 700.0,
                lockout_ms: 25.0,
                stacked_release_gap_ms: 35.0,
                suppress_normal_up_after_force_ms: 80.0,
            },
            drag_release: DragReleaseConfig {
                enabled: true,
                movement: 30.0,
                threshold: 30,
                debug: false,
            },
            haptics: HapticsConfig {
                down_param: MEDIUM_DOWN_PARAM,
                up_param: MEDIUM_UP_PARAM,
                normal_down_byte3: 0x17,
                normal_up_byte3: 0x14,
                force_down_byte3: 0x17,
                force_up_byte3: 0x14,
            },
            drag: DragConfig {
                enabled: false,
                mode: DragMode::Any,
                distance: 12.0,
                two_finger_distance: None,
                three_finger_distance: None,
                min_gap_ms: 40.0,
                two_finger_min_gap_ms: None,
                three_finger_min_gap_ms: None,
                down_ms: 4.0,
                motion_epsilon: 2.0,
                click_silence_ms: 0.0,
                max_pressure: None,
                debug: false,
            },
            ridge: RidgeConfig {
                enabled: false,
                position: 0.0,
                grid_columns: 1,
                grid_rows: 1,
                grid_x_min: -3600.0,
                grid_x_max: 3600.0,
                grid_y_min: -2400.0,
                grid_y_max: 2400.0,
                deadband: 80.0,
                min_gap_ms: 80.0,
                down_ms: 4.0,
                pulses: 1,
                pulse_gap_ms: 10.0,
                down_param: MEDIUM_DOWN_PARAM,
                up_param: MEDIUM_UP_PARAM,
                down_byte3: 0x17,
                up_byte3: 0x14,
                debug: false,
            },
        }
    }
}

#[derive(Debug)]
struct Args {
    config: Option<PathBuf>,
    device: Option<String>,
    dry_run: bool,
    force: bool,
    no_input: bool,
    no_restore: bool,
    force_button: Option<Option<u16>>,
    pending_left_ms: Option<f64>,
    pending_left_motion: Option<f64>,
    double_click_force_threshold: Option<i32>,
    double_click_window_ms: Option<f64>,
    normal_release_threshold: Option<i32>,
    drag_release_threshold: Option<i32>,
    drag_release_motion: Option<f64>,
    seconds: Option<f64>,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut args = env::args().skip(1);
        let mut parsed = Self {
            config: None,
            device: None,
            dry_run: false,
            force: false,
            no_input: false,
            no_restore: false,
            force_button: None,
            pending_left_ms: None,
            pending_left_motion: None,
            double_click_force_threshold: None,
            double_click_window_ms: None,
            normal_release_threshold: None,
            drag_release_threshold: None,
            drag_release_motion: None,
            seconds: None,
        };

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                "--config" => {
                    parsed.config = Some(PathBuf::from(next_arg(&mut args, "--config")?));
                }
                "--device" => {
                    parsed.device = Some(next_arg(&mut args, "--device")?);
                }
                "--dry-run" => parsed.dry_run = true,
                "--force" => parsed.force = true,
                "--no-input" | "--no-emit-input" => parsed.no_input = true,
                "--no-restore" => parsed.no_restore = true,
                "--force-button" => {
                    let value = next_arg(&mut args, "--force-button")?;
                    parsed.force_button = Some(parse_button_name("--force-button", &value)?);
                }
                "--pending-left-ms" => {
                    let value = next_arg(&mut args, "--pending-left-ms")?;
                    parsed.pending_left_ms = Some(parse_f64("--pending-left-ms", &value)?);
                }
                "--pending-left-motion" => {
                    let value = next_arg(&mut args, "--pending-left-motion")?;
                    parsed.pending_left_motion = Some(parse_f64("--pending-left-motion", &value)?);
                }
                "--double-click-force-threshold" => {
                    let value = next_arg(&mut args, "--double-click-force-threshold")?;
                    parsed.double_click_force_threshold =
                        Some(parse_i32("--double-click-force-threshold", &value)?);
                }
                "--double-click-window-ms" => {
                    let value = next_arg(&mut args, "--double-click-window-ms")?;
                    parsed.double_click_window_ms =
                        Some(parse_f64("--double-click-window-ms", &value)?);
                }
                "--normal-release-threshold" => {
                    let value = next_arg(&mut args, "--normal-release-threshold")?;
                    parsed.normal_release_threshold =
                        Some(parse_i32("--normal-release-threshold", &value)?);
                }
                "--drag-release-threshold" => {
                    let value = next_arg(&mut args, "--drag-release-threshold")?;
                    parsed.drag_release_threshold =
                        Some(parse_i32("--drag-release-threshold", &value)?);
                }
                "--drag-release-motion" => {
                    let value = next_arg(&mut args, "--drag-release-motion")?;
                    parsed.drag_release_motion = Some(parse_f64("--drag-release-motion", &value)?);
                }
                "--seconds" => {
                    let value = next_arg(&mut args, "--seconds")?;
                    parsed.seconds = Some(parse_f64("--seconds", &value)?);
                }
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        Ok(parsed)
    }
}

fn next_arg(args: &mut impl Iterator<Item = String>, name: &str) -> Result<String> {
    args.next()
        .ok_or_else(|| format!("{name} requires a value"))
}

fn print_help() {
    println!(
        "force-touchd\n\n\
         Usage: force-touchd [--config PATH] [--device HIDRAW] [--dry-run]\n\n\
         Options:\n\
           --config PATH       TOML config path\n\
           --device HIDRAW     Override configured device path\n\
           --dry-run           Print resolved config and reports without touching hardware\n\
           --force             Allow non-Magic Trackpad 2 HID IDs\n\
           --force-button BTN  Button emitted by force click: left, right, middle, none\n\
           --pending-left-ms N Delay one-finger BTN_LEFT down so force can replace it\n\
           --pending-left-motion N\n\
                                Commit pending BTN_LEFT after this centroid movement\n\
           --double-click-force-threshold N\n\
                                Harder force threshold after a double-click hold\n\
           --double-click-window-ms N\n\
                                Max time between click release and next click for double-click guard\n\
           --normal-release-threshold N\n\
                                Release non-drag normal input at/below this pressure\n\
           --drag-release-threshold N\n\
                                Release drag normal input at/below this pressure\n\
           --drag-release-motion N\n\
                                Movement that marks a normal click as a drag\n\
           --no-input          Disable /dev/uinput mouse events\n\
           --no-restore        Leave host-click mode enabled on exit\n\
           --seconds N         Stop after N seconds\n"
    );
}

fn parse_f64(name: &str, value: &str) -> Result<f64> {
    value.parse::<f64>().map_err(|err| format!("{name}: {err}"))
}

fn parse_i32(name: &str, value: &str) -> Result<i32> {
    value.parse::<i32>().map_err(|err| format!("{name}: {err}"))
}

fn default_config_path() -> Option<PathBuf> {
    let local = PathBuf::from(LOCAL_CONFIG_PATH);
    if local.exists() {
        return Some(local);
    }
    let system = PathBuf::from(SYSTEM_CONFIG_PATH);
    if system.exists() {
        return Some(system);
    }
    None
}

fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else if ch == '"' {
            in_string = true;
        } else if ch == '#' {
            return &line[..index];
        }
    }
    line
}

#[derive(Clone, Debug)]
enum ConfigValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
}

fn parse_config_value(raw: &str, line: usize) -> Result<ConfigValue> {
    let value = raw.trim();
    if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        return Ok(ConfigValue::String(value[1..value.len() - 1].to_string()));
    }
    if value == "true" {
        return Ok(ConfigValue::Bool(true));
    }
    if value == "false" {
        return Ok(ConfigValue::Bool(false));
    }
    if value.contains('.') || value.contains('e') || value.contains('E') {
        return value
            .parse::<f64>()
            .map(ConfigValue::Float)
            .map_err(|err| format!("line {line}: invalid float {value:?}: {err}"));
    }
    let parsed = if let Some(hex) = value.strip_prefix("0x") {
        i64::from_str_radix(hex, 16)
    } else {
        value.parse::<i64>()
    };
    parsed
        .map(ConfigValue::Int)
        .map_err(|err| format!("line {line}: invalid value {value:?}: {err}"))
}

fn value_bool(key: &str, value: ConfigValue) -> Result<bool> {
    match value {
        ConfigValue::Bool(value) => Ok(value),
        _ => Err(format!("{key} must be a boolean")),
    }
}

fn value_string(key: &str, value: ConfigValue) -> Result<String> {
    match value {
        ConfigValue::String(value) => Ok(value),
        _ => Err(format!("{key} must be a string")),
    }
}

fn value_i32(key: &str, value: ConfigValue) -> Result<i32> {
    match value {
        ConfigValue::Int(value) if i32::try_from(value).is_ok() => Ok(value as i32),
        _ => Err(format!("{key} must be a 32-bit integer")),
    }
}

fn value_u32(key: &str, value: ConfigValue) -> Result<u32> {
    match value {
        ConfigValue::Int(value) if u32::try_from(value).is_ok() => Ok(value as u32),
        _ => Err(format!("{key} must be a 32-bit unsigned integer")),
    }
}

fn value_u8(key: &str, value: ConfigValue) -> Result<u8> {
    match value {
        ConfigValue::Int(value) if u8::try_from(value).is_ok() => Ok(value as u8),
        _ => Err(format!("{key} must be a byte integer")),
    }
}

fn value_f64(key: &str, value: ConfigValue) -> Result<f64> {
    match value {
        ConfigValue::Float(value) => Ok(value),
        ConfigValue::Int(value) => Ok(value as f64),
        _ => Err(format!("{key} must be numeric")),
    }
}

fn parse_button_name(key: &str, value: &str) -> Result<Option<u16>> {
    match value {
        "left" => Ok(Some(BTN_LEFT)),
        "right" => Ok(Some(BTN_RIGHT)),
        "middle" => Ok(Some(BTN_MIDDLE)),
        "none" => Ok(None),
        _ => Err(format!("{key} must be left, right, middle, or none")),
    }
}

fn apply_config_value(config: &mut Config, key: &str, value: ConfigValue) -> Result<()> {
    match key {
        "device.path" => {
            let path = value_string(key, value)?;
            config.device.path = if path == "auto" { None } else { Some(path) };
        }
        "device.restore_on_exit" => config.device.restore_on_exit = value_bool(key, value)?,
        "device.settle_ms" => config.device.settle_ms = value_f64(key, value)?,
        "input.enabled" => config.input.enabled = value_bool(key, value)?,
        "input.force_button" => {
            config.input.force_button = parse_button_name(key, &value_string(key, value)?)?;
        }
        "input.pending_left_ms" => config.input.pending_left_ms = value_f64(key, value)?,
        "input.pending_left_motion" => config.input.pending_left_motion = value_f64(key, value)?,
        "input.multi_touch_drag_motion" => {
            config.input.multi_touch_drag_motion = value_bool(key, value)?;
        }
        "input.multi_touch_motion_x_scale" => {
            config.input.multi_touch_motion_x_scale = value_f64(key, value)?;
        }
        "input.multi_touch_motion_y_scale" => {
            config.input.multi_touch_motion_y_scale = value_f64(key, value)?;
        }
        "input.multi_touch_motion_debug" => {
            config.input.multi_touch_motion_debug = value_bool(key, value)?;
        }
        "clicks.normal_threshold" => config.clicks.normal_threshold = value_i32(key, value)?,
        "clicks.force_threshold" => config.clicks.force_threshold = value_i32(key, value)?,
        "clicks.double_click_force_threshold" => {
            config.clicks.double_click_force_threshold = value_i32(key, value)?;
        }
        "clicks.double_click_window_ms" => {
            config.clicks.double_click_window_ms = value_f64(key, value)?;
        }
        "clicks.reset_threshold" => config.clicks.reset_threshold = value_i32(key, value)?,
        "clicks.rearm_threshold" => config.clicks.rearm_threshold = value_i32(key, value)?,
        "clicks.force_rearm_threshold" => {
            config.clicks.force_rearm_threshold = value_i32(key, value)?;
        }
        "clicks.normal_release_threshold" => {
            config.clicks.normal_release_threshold = value_i32(key, value)?;
        }
        "clicks.drag_release_threshold" => {
            config.drag_release.threshold = value_i32(key, value)?;
        }
        "clicks.drag_release_motion" => {
            config.drag_release.movement = value_f64(key, value)?;
        }
        "clicks.min_gap_ms" => config.clicks.min_gap_ms = value_f64(key, value)?,
        "release.drop" => config.release.drop = value_i32(key, value)?,
        "release.rate" => config.release.rate = value_f64(key, value)?,
        "release.lockout_ms" => config.release.lockout_ms = value_f64(key, value)?,
        "release.stacked_release_gap_ms" => {
            config.release.stacked_release_gap_ms = value_f64(key, value)?;
        }
        "release.suppress_normal_up_after_force_ms" => {
            config.release.suppress_normal_up_after_force_ms = value_f64(key, value)?;
        }
        "drag_release.enabled" => config.drag_release.enabled = value_bool(key, value)?,
        "drag_release.movement" => config.drag_release.movement = value_f64(key, value)?,
        "drag_release.threshold" => config.drag_release.threshold = value_i32(key, value)?,
        "drag_release.debug" => config.drag_release.debug = value_bool(key, value)?,
        "haptics.down_param" => config.haptics.down_param = value_u32(key, value)?,
        "haptics.up_param" => config.haptics.up_param = value_u32(key, value)?,
        "haptics.normal.down_byte3" => {
            config.haptics.normal_down_byte3 = value_u8(key, value)?;
        }
        "haptics.normal.up_byte3" => {
            config.haptics.normal_up_byte3 = value_u8(key, value)?;
        }
        "haptics.force.down_byte3" => {
            config.haptics.force_down_byte3 = value_u8(key, value)?;
        }
        "haptics.force.up_byte3" => {
            config.haptics.force_up_byte3 = value_u8(key, value)?;
        }
        "drag_haptics.enabled" => config.drag.enabled = value_bool(key, value)?,
        "drag_haptics.mode" => {
            config.drag.mode = DragMode::parse(&value_string(key, value)?)?;
        }
        "drag_haptics.distance" => config.drag.distance = value_f64(key, value)?,
        "drag_haptics.two_finger_distance" => {
            config.drag.two_finger_distance = Some(value_f64(key, value)?);
        }
        "drag_haptics.three_finger_distance" => {
            config.drag.three_finger_distance = Some(value_f64(key, value)?);
        }
        "drag_haptics.min_gap_ms" => config.drag.min_gap_ms = value_f64(key, value)?,
        "drag_haptics.two_finger_min_gap_ms" => {
            config.drag.two_finger_min_gap_ms = Some(value_f64(key, value)?);
        }
        "drag_haptics.three_finger_min_gap_ms" => {
            config.drag.three_finger_min_gap_ms = Some(value_f64(key, value)?);
        }
        "drag_haptics.down_ms" => config.drag.down_ms = value_f64(key, value)?,
        "drag_haptics.motion_epsilon" => config.drag.motion_epsilon = value_f64(key, value)?,
        "drag_haptics.click_silence_ms" => {
            config.drag.click_silence_ms = value_f64(key, value)?;
        }
        "drag_haptics.max_pressure" => config.drag.max_pressure = Some(value_i32(key, value)?),
        "drag_haptics.debug" => config.drag.debug = value_bool(key, value)?,
        "ridge_haptics.enabled" => config.ridge.enabled = value_bool(key, value)?,
        "ridge_haptics.position" => config.ridge.position = value_f64(key, value)?,
        "ridge_haptics.grid_columns" => config.ridge.grid_columns = value_i32(key, value)?,
        "ridge_haptics.grid_rows" => config.ridge.grid_rows = value_i32(key, value)?,
        "ridge_haptics.grid_x_min" => config.ridge.grid_x_min = value_f64(key, value)?,
        "ridge_haptics.grid_x_max" => config.ridge.grid_x_max = value_f64(key, value)?,
        "ridge_haptics.grid_y_min" => config.ridge.grid_y_min = value_f64(key, value)?,
        "ridge_haptics.grid_y_max" => config.ridge.grid_y_max = value_f64(key, value)?,
        "ridge_haptics.deadband" => config.ridge.deadband = value_f64(key, value)?,
        "ridge_haptics.min_gap_ms" => config.ridge.min_gap_ms = value_f64(key, value)?,
        "ridge_haptics.down_ms" => config.ridge.down_ms = value_f64(key, value)?,
        "ridge_haptics.pulses" => config.ridge.pulses = value_i32(key, value)?,
        "ridge_haptics.pulse_gap_ms" => config.ridge.pulse_gap_ms = value_f64(key, value)?,
        "ridge_haptics.down_param" => config.ridge.down_param = value_u32(key, value)?,
        "ridge_haptics.up_param" => config.ridge.up_param = value_u32(key, value)?,
        "ridge_haptics.down_byte3" => config.ridge.down_byte3 = value_u8(key, value)?,
        "ridge_haptics.up_byte3" => config.ridge.up_byte3 = value_u8(key, value)?,
        "ridge_haptics.debug" => config.ridge.debug = value_bool(key, value)?,
        _ => return Err(format!("unknown config key: {key}")),
    }
    Ok(())
}

fn load_config_file(config: &mut Config, path: &Path) -> Result<()> {
    let content = fs::read_to_string(path)
        .map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    let mut section = String::new();
    for (index, raw_line) in content.lines().enumerate() {
        let line_no = index + 1;
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {line_no}: expected key = value"));
        };
        let key = key.trim();
        let full_key = if section.is_empty() {
            key.to_string()
        } else {
            format!("{section}.{key}")
        };
        let value = parse_config_value(value, line_no)?;
        apply_config_value(config, &full_key, value)?;
    }
    Ok(())
}

fn validate_config(config: &Config) -> Result<()> {
    if !(config.clicks.reset_threshold < config.clicks.rearm_threshold
        && config.clicks.rearm_threshold < config.clicks.normal_threshold
        && config.clicks.normal_threshold < config.clicks.force_rearm_threshold
        && config.clicks.force_rearm_threshold < config.clicks.force_threshold)
    {
        return Err(
            "click thresholds must be strictly increasing through force_threshold".to_string(),
        );
    }
    if config.clicks.double_click_force_threshold < config.clicks.force_threshold {
        return Err(
            "clicks.double_click_force_threshold must be at least clicks.force_threshold"
                .to_string(),
        );
    }
    if config.clicks.double_click_window_ms < 0.0 {
        return Err("clicks.double_click_window_ms must not be negative".to_string());
    }
    if config.input.pending_left_ms < 0.0 {
        return Err("input.pending_left_ms must not be negative".to_string());
    }
    if config.input.pending_left_motion < 0.0 {
        return Err("input.pending_left_motion must not be negative".to_string());
    }
    if !config.input.multi_touch_motion_x_scale.is_finite() {
        return Err("input.multi_touch_motion_x_scale must be finite".to_string());
    }
    if !config.input.multi_touch_motion_y_scale.is_finite() {
        return Err("input.multi_touch_motion_y_scale must be finite".to_string());
    }
    if config.clicks.normal_release_threshold < 0
        || config.clicks.normal_release_threshold >= config.clicks.normal_threshold
    {
        return Err(
            "clicks.normal_release_threshold must be below clicks.normal_threshold".to_string(),
        );
    }
    if config.drag_release.enabled {
        if config.drag_release.movement < 1.0 {
            return Err("drag_release.movement must be at least 1".to_string());
        }
        if !(0..=255).contains(&config.drag_release.threshold) {
            return Err("drag_release.threshold must be in byte range 0..255".to_string());
        }
        if config.drag_release.threshold >= config.clicks.normal_threshold {
            return Err("drag_release.threshold must be below clicks.normal_threshold".to_string());
        }
    }
    if config.drag.enabled {
        let intervals = [
            ("drag_haptics.min_gap_ms", config.drag.min_gap_ms),
            (
                "drag_haptics.two_finger_min_gap_ms",
                config
                    .drag
                    .two_finger_min_gap_ms
                    .unwrap_or(config.drag.min_gap_ms),
            ),
            (
                "drag_haptics.three_finger_min_gap_ms",
                config
                    .drag
                    .three_finger_min_gap_ms
                    .unwrap_or(config.drag.min_gap_ms),
            ),
        ];
        for (name, interval) in intervals {
            if interval < 20.0 {
                return Err(format!("{name} must be at least 20"));
            }
            if config.drag.down_ms >= interval {
                return Err(format!("drag_haptics.down_ms must be smaller than {name}"));
            }
        }
        for (name, distance) in [
            ("drag_haptics.distance", config.drag.distance),
            (
                "drag_haptics.two_finger_distance",
                config
                    .drag
                    .two_finger_distance
                    .unwrap_or(config.drag.distance),
            ),
            (
                "drag_haptics.three_finger_distance",
                config
                    .drag
                    .three_finger_distance
                    .unwrap_or(config.drag.distance),
            ),
        ] {
            if distance < 1.0 {
                return Err(format!("{name} must be at least 1"));
            }
        }
        if config.drag.motion_epsilon < 0.0 {
            return Err("drag_haptics.motion_epsilon must not be negative".to_string());
        }
        if config.drag.click_silence_ms < 0.0 {
            return Err("drag_haptics.click_silence_ms must not be negative".to_string());
        }
        if let Some(max_pressure) = config.drag.max_pressure {
            if !(0..=255).contains(&max_pressure) {
                return Err("drag_haptics.max_pressure must be in byte range 0..255".to_string());
            }
        }
    }
    if config.ridge.enabled {
        if config.ridge.deadband < 1.0 {
            return Err("ridge_haptics.deadband must be at least 1".to_string());
        }
        if !(1..=16).contains(&config.ridge.grid_columns) {
            return Err("ridge_haptics.grid_columns must be between 1 and 16".to_string());
        }
        if !(1..=16).contains(&config.ridge.grid_rows) {
            return Err("ridge_haptics.grid_rows must be between 1 and 16".to_string());
        }
        if config.ridge.grid_x_min >= config.ridge.grid_x_max {
            return Err("ridge_haptics.grid_x_min must be less than grid_x_max".to_string());
        }
        if config.ridge.grid_y_min >= config.ridge.grid_y_max {
            return Err("ridge_haptics.grid_y_min must be less than grid_y_max".to_string());
        }
        let grid_cell_width = (config.ridge.grid_x_max - config.ridge.grid_x_min)
            / f64::from(config.ridge.grid_columns);
        let grid_cell_height =
            (config.ridge.grid_y_max - config.ridge.grid_y_min) / f64::from(config.ridge.grid_rows);
        if config.ridge.grid_columns > 1 && config.ridge.deadband >= grid_cell_width {
            return Err(
                "ridge_haptics.deadband must be smaller than grid column width".to_string(),
            );
        }
        if config.ridge.grid_rows > 1 && config.ridge.deadband >= grid_cell_height {
            return Err("ridge_haptics.deadband must be smaller than grid row height".to_string());
        }
        if config.ridge.min_gap_ms < 20.0 {
            return Err("ridge_haptics.min_gap_ms must be at least 20".to_string());
        }
        if config.ridge.down_ms >= config.ridge.min_gap_ms {
            return Err(
                "ridge_haptics.down_ms must be smaller than ridge_haptics.min_gap_ms".to_string(),
            );
        }
        if !(1..=3).contains(&config.ridge.pulses) {
            return Err("ridge_haptics.pulses must be between 1 and 3".to_string());
        }
        if config.ridge.pulse_gap_ms < 5.0 {
            return Err("ridge_haptics.pulse_gap_ms must be at least 5".to_string());
        }
        let burst_ms = config.ridge.down_ms * f64::from(config.ridge.pulses)
            + config.ridge.pulse_gap_ms * f64::from(config.ridge.pulses - 1);
        if burst_ms >= config.ridge.min_gap_ms {
            return Err(
                "ridge haptic burst must be shorter than ridge_haptics.min_gap_ms".to_string(),
            );
        }
    }
    Ok(())
}

fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

fn format_optional_ms(value: Option<f64>) -> String {
    value.map_or_else(
        || "base".to_string(),
        |value| format!("{}ms", format_number(value)),
    )
}

fn print_resolved_config(config: &Config, device: &str) {
    println!("device: {device}");
    println!(
        "thresholds: normal >= {}; force >= {}; double-click force >= {} within {}ms; force re-arm <= {}; normal release <= {}; re-arm <= {}; reset <= {}",
        config.clicks.normal_threshold,
        config.clicks.force_threshold,
        config.clicks.double_click_force_threshold,
        format_number(config.clicks.double_click_window_ms),
        config.clicks.force_rearm_threshold,
        config.clicks.normal_release_threshold,
        config.clicks.rearm_threshold,
        config.clicks.reset_threshold
    );
    println!(
        "release: drop >= {}; rate >= {}/s; lockout {}ms; stack gap {}ms; suppress normal-up {}ms",
        config.release.drop,
        format_number(config.release.rate),
        format_number(config.release.lockout_ms),
        format_number(config.release.stacked_release_gap_ms),
        format_number(config.release.suppress_normal_up_after_force_ms)
    );
    println!(
        "drag release: {}",
        if config.drag_release.enabled {
            format!(
                "enabled movement={} release <= {}",
                format_number(config.drag_release.movement),
                config.drag_release.threshold
            )
        } else {
            "disabled".to_string()
        }
    );
    println!(
        "input: {}",
        if config.input.enabled {
            format!(
                "{}; pending-left={}ms motion={}; multi-touch drag motion={} x-scale={} y-scale={}",
                input_summary(config.input.force_button),
                format_number(config.input.pending_left_ms),
                format_number(config.input.pending_left_motion),
                if config.input.multi_touch_drag_motion {
                    "enabled"
                } else {
                    "disabled"
                },
                format_number(config.input.multi_touch_motion_x_scale),
                format_number(config.input.multi_touch_motion_y_scale)
            )
        } else {
            "disabled".to_string()
        }
    );
    println!(
        "drag haptics: {}",
        if config.drag.enabled {
            format!(
                "enabled min-gap={}ms 2f-min-gap={} 3f-min-gap={} down={}ms distance={} 2f-distance={} 3f-distance={} motion-epsilon={} mode={} max-pressure={} click-silence={}ms",
                format_number(config.drag.min_gap_ms),
                format_optional_ms(config.drag.two_finger_min_gap_ms),
                format_optional_ms(config.drag.three_finger_min_gap_ms),
                format_number(config.drag.down_ms),
                format_number(config.drag.distance),
                config
                    .drag
                    .two_finger_distance
                    .map_or_else(|| "base".to_string(), format_number),
                config
                    .drag
                    .three_finger_distance
                    .map_or_else(|| "base".to_string(), format_number),
                format_number(config.drag.motion_epsilon),
                config.drag.mode.as_str(),
                config
                    .drag
                    .max_pressure
                    .map_or_else(|| "off".to_string(), |value| value.to_string()),
                format_number(config.drag.click_silence_ms),
            )
        } else {
            "disabled".to_string()
        }
    );
    println!(
        "ridge haptics: {}",
        if config.ridge.enabled {
            if config.ridge.grid_columns > 1 || config.ridge.grid_rows > 1 {
                format!(
                    "enabled grid={}x{} x={}..{} y={}..{} deadband={} min-gap={}ms down={}ms pulses={} pulse-gap={}ms down-param={:#010x} down-byte3={:#04x}",
                    config.ridge.grid_columns,
                    config.ridge.grid_rows,
                    format_number(config.ridge.grid_x_min),
                    format_number(config.ridge.grid_x_max),
                    format_number(config.ridge.grid_y_min),
                    format_number(config.ridge.grid_y_max),
                    format_number(config.ridge.deadband),
                    format_number(config.ridge.min_gap_ms),
                    format_number(config.ridge.down_ms),
                    config.ridge.pulses,
                    format_number(config.ridge.pulse_gap_ms),
                    config.ridge.down_param,
                    config.ridge.down_byte3,
                )
            } else {
                format!(
                    "enabled x={} deadband={} min-gap={}ms down={}ms pulses={} pulse-gap={}ms down-param={:#010x} down-byte3={:#04x}",
                    format_number(config.ridge.position),
                    format_number(config.ridge.deadband),
                    format_number(config.ridge.min_gap_ms),
                    format_number(config.ridge.down_ms),
                    config.ridge.pulses,
                    format_number(config.ridge.pulse_gap_ms),
                    config.ridge.down_param,
                    config.ridge.down_byte3,
                )
            }
        } else {
            "disabled".to_string()
        }
    );
}

fn ioc(direction: u64, type_: u8, number: u64, size: u64) -> c_ulong {
    ((direction << IOC_DIRSHIFT)
        | ((type_ as u64) << IOC_TYPESHIFT)
        | (number << IOC_NRSHIFT)
        | (size << IOC_SIZESHIFT)) as c_ulong
}

fn hid_iocsfeature(length: usize) -> c_ulong {
    ioc(IOC_WRITE | IOC_READ, b'H', 0x06, length as u64)
}

fn hid_iocgrawinfo() -> c_ulong {
    ioc(IOC_READ, b'H', 0x03, 8)
}

fn ui_set_evbit() -> c_ulong {
    ioc(IOC_WRITE, b'U', 100, 4)
}

fn ui_set_keybit() -> c_ulong {
    ioc(IOC_WRITE, b'U', 101, 4)
}

fn ui_set_relbit() -> c_ulong {
    ioc(IOC_WRITE, b'U', 102, 4)
}

fn ui_dev_create() -> c_ulong {
    ioc(IOC_NONE, b'U', 1, 0)
}

fn ui_dev_destroy() -> c_ulong {
    ioc(IOC_NONE, b'U', 2, 0)
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawInfo {
    bus: u32,
    vendor: u16,
    product: u16,
}

fn ioctl_result(result: c_int, context: &str) -> Result<()> {
    if result < 0 {
        Err(format!("{context}: {}", std::io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

fn get_raw_info(fd: RawFd) -> Result<RawInfo> {
    let mut info = RawInfo::default();
    let result = unsafe { ioctl(fd, hid_iocgrawinfo(), &mut info) };
    ioctl_result(result, "HIDIOCGRAWINFO")?;
    Ok(info)
}

fn ensure_supported(file: &File, device: &str, force: bool) -> Result<()> {
    let info = get_raw_info(file.as_raw_fd())?;
    let supported = info.bus == BUS_BLUETOOTH
        && info.vendor == APPLE_BT_VENDOR
        && info.product == MAGIC_TRACKPAD2_PRODUCT;
    if !supported && !force {
        return Err(format!(
            "{device} is bus={:#06x} vendor={:#06x} product={:#06x}, not Bluetooth Magic Trackpad 2",
            info.bus, info.vendor, info.product
        ));
    }
    Ok(())
}

fn set_feature_fd(fd: RawFd, report: &[u8]) -> Result<()> {
    let mut report = report.to_vec();
    let result = unsafe { ioctl(fd, hid_iocsfeature(report.len()), report.as_mut_ptr()) };
    ioctl_result(result, "HIDIOCSFEATURE")
}

fn write_output(file: &mut File, report: &[u8]) -> Result<()> {
    file.write_all(report)
        .map_err(|err| format!("hidraw write failed: {err}"))
}

struct HostClickGuard {
    fd: RawFd,
    restore: bool,
}

impl Drop for HostClickGuard {
    fn drop(&mut self) {
        if self.restore {
            if let Err(err) = set_feature_fd(self.fd, &HOST_CLICK_OFF) {
                eprintln!("warning: failed to restore default click mode: {err}");
            } else {
                println!("host-click off: {}", format_bytes(&HOST_CLICK_OFF));
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Touch {
    tracking_id: u8,
    x: i32,
    y: i32,
    pressure: i32,
    state: u8,
}

impl Touch {
    fn down(self) -> bool {
        self.state == 0x80
    }
}

fn signed(value: u32, bits: u8) -> i32 {
    let sign_bit = 1_u32 << (bits - 1);
    let mask = (1_u32 << bits) - 1;
    let value = value & mask;
    if value & sign_bit != 0 {
        value as i32 - (1_i32 << bits)
    } else {
        value as i32
    }
}

fn parse_touches(data: &[u8]) -> Vec<Touch> {
    if data.is_empty() || data[0] != 0x31 || data.len() < 4 || (data.len() - 4) % 9 != 0 {
        return Vec::new();
    }
    let mut touches = Vec::new();
    for touch in data[4..].chunks_exact(9) {
        let x = signed(((touch[1] as u32) << 8) | touch[0] as u32, 13);
        let y = -signed(
            ((touch[1] as u32) >> 5)
                | ((touch[2] as u32) << 3)
                | (((touch[3] & 0x03) as u32) << 11),
            13,
        );
        touches.push(Touch {
            tracking_id: touch[8] & 0x0f,
            x,
            y,
            pressure: touch[7] as i32,
            state: touch[3] & 0xc0,
        });
    }
    touches
}

fn touch_centroid(touches: &[Touch]) -> Option<(f64, f64)> {
    if touches.is_empty() {
        return None;
    }
    let x_sum: i32 = touches.iter().map(|touch| touch.x).sum();
    let y_sum: i32 = touches.iter().map(|touch| touch.y).sum();
    Some((
        x_sum as f64 / touches.len() as f64,
        y_sum as f64 / touches.len() as f64,
    ))
}

fn button_for_touch_count(touch_count: usize) -> u16 {
    if touch_count >= 3 {
        BTN_MIDDLE
    } else if touch_count == 2 {
        BTN_RIGHT
    } else {
        BTN_LEFT
    }
}

fn button_name(button: u16) -> &'static str {
    match button {
        BTN_LEFT => "left",
        BTN_RIGHT => "right",
        BTN_MIDDLE => "middle",
        _ => "unknown",
    }
}

fn optional_button_name(button: Option<u16>) -> &'static str {
    button.map_or("none", button_name)
}

fn input_summary(force_button: Option<u16>) -> String {
    format!(
        "1-finger=left, 2-finger=right, 3+-finger=middle, force={}",
        optional_button_name(force_button)
    )
}

#[repr(C)]
#[derive(Clone, Copy)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UInputUserDev {
    name: [u8; 80],
    id: InputId,
    ff_effects_max: u32,
    absmax: [i32; 64],
    absmin: [i32; 64],
    absfuzz: [i32; 64],
    absflat: [i32; 64],
}

#[repr(C)]
struct InputEvent {
    tv_sec: i64,
    tv_usec: i64,
    type_: u16,
    code: u16,
    value: i32,
}

fn write_struct<T>(file: &mut File, value: &T) -> std::io::Result<()> {
    let bytes =
        unsafe { std::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) };
    file.write_all(bytes)
}

struct VirtualMouse {
    file: Option<File>,
    normal_button: Option<u16>,
    force_button: Option<u16>,
    force_button_down: Option<u16>,
}

impl VirtualMouse {
    fn new(enabled: bool, force_button: Option<u16>) -> Result<Self> {
        let file = if enabled {
            Some(Self::open_uinput_mouse()?)
        } else {
            None
        };
        Ok(Self {
            file,
            normal_button: None,
            force_button,
            force_button_down: None,
        })
    }

    fn open_uinput_mouse() -> Result<File> {
        let mut file = OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .map_err(|err| format!("failed to open /dev/uinput: {err}"))?;
        let fd = file.as_raw_fd();
        ioctl_result(
            unsafe { ioctl(fd, ui_set_evbit(), EV_KEY as c_int) },
            "UI_SET_EVBIT EV_KEY",
        )?;
        ioctl_result(
            unsafe { ioctl(fd, ui_set_evbit(), EV_REL as c_int) },
            "UI_SET_EVBIT EV_REL",
        )?;
        ioctl_result(
            unsafe { ioctl(fd, ui_set_keybit(), BTN_LEFT as c_int) },
            "UI_SET_KEYBIT BTN_LEFT",
        )?;
        ioctl_result(
            unsafe { ioctl(fd, ui_set_keybit(), BTN_RIGHT as c_int) },
            "UI_SET_KEYBIT BTN_RIGHT",
        )?;
        ioctl_result(
            unsafe { ioctl(fd, ui_set_keybit(), BTN_MIDDLE as c_int) },
            "UI_SET_KEYBIT BTN_MIDDLE",
        )?;
        ioctl_result(
            unsafe { ioctl(fd, ui_set_relbit(), REL_X as c_int) },
            "UI_SET_RELBIT REL_X",
        )?;
        ioctl_result(
            unsafe { ioctl(fd, ui_set_relbit(), REL_Y as c_int) },
            "UI_SET_RELBIT REL_Y",
        )?;

        let mut name = [0_u8; 80];
        let device_name = b"ForceTouchLinux virtual mouse";
        name[..device_name.len()].copy_from_slice(device_name);
        let user_dev = UInputUserDev {
            name,
            id: InputId {
                bustype: BUS_VIRTUAL,
                vendor: APPLE_BT_VENDOR,
                product: MAGIC_TRACKPAD2_PRODUCT,
                version: 1,
            },
            ff_effects_max: 0,
            absmax: [0; 64],
            absmin: [0; 64],
            absfuzz: [0; 64],
            absflat: [0; 64],
        };
        write_struct(&mut file, &user_dev).map_err(|err| format!("uinput write failed: {err}"))?;
        ioctl_result(unsafe { ioctl(fd, ui_dev_create(), 0) }, "UI_DEV_CREATE")?;
        thread::sleep(Duration::from_millis(100));
        Ok(file)
    }

    fn emit(&mut self, event_type: u16, code: u16, value: i32) -> Result<()> {
        let Some(file) = self.file.as_mut() else {
            return Ok(());
        };
        let event = InputEvent {
            tv_sec: 0,
            tv_usec: 0,
            type_: event_type,
            code,
            value,
        };
        write_struct(file, &event).map_err(|err| format!("uinput event write failed: {err}"))
    }

    fn sync(&mut self) -> Result<()> {
        self.emit(EV_SYN, SYN_REPORT, 0)
    }

    fn key(&mut self, code: u16, pressed: bool) -> Result<()> {
        self.emit(EV_KEY, code, if pressed { 1 } else { 0 })?;
        self.sync()
    }

    fn rel_motion(&mut self, dx: i32, dy: i32) -> Result<()> {
        if dx == 0 && dy == 0 {
            return Ok(());
        }
        if dx != 0 {
            self.emit(EV_REL, REL_X, dx)?;
        }
        if dy != 0 {
            self.emit(EV_REL, REL_Y, dy)?;
        }
        self.sync()
    }

    fn normal_down(&mut self, button: u16) -> Result<()> {
        if self.file.is_some() && self.normal_button.is_none() && self.force_button_down.is_none() {
            self.key(button, true)?;
            self.normal_button = Some(button);
        }
        Ok(())
    }

    fn normal_up(&mut self) -> Result<()> {
        if let Some(button) = self.normal_button.take() {
            self.key(button, false)?;
        }
        Ok(())
    }

    fn force_down(&mut self) -> Result<()> {
        let Some(force_button) = self.force_button else {
            return Ok(());
        };
        if self.file.is_none() || self.force_button_down.is_some() {
            return Ok(());
        }
        if self.normal_button == Some(BTN_LEFT) {
            if force_button == BTN_LEFT {
                return Ok(());
            }
        } else if self.normal_button.is_some() {
            return Ok(());
        }
        self.key(force_button, true)?;
        self.force_button_down = Some(force_button);
        Ok(())
    }

    fn force_up(&mut self) -> Result<()> {
        if let Some(button) = self.force_button_down.take() {
            self.key(button, false)?;
        }
        Ok(())
    }
}

impl Drop for VirtualMouse {
    fn drop(&mut self) {
        let _ = self.force_up();
        let _ = self.normal_up();
        if let Some(file) = self.file.as_ref() {
            let _ = unsafe { ioctl(file.as_raw_fd(), ui_dev_destroy(), 0) };
        }
    }
}

fn haptic_report(template: [u8; 15], param: u32) -> [u8; 15] {
    let mut report = template;
    report[3] = ((param >> 16) & 0xff) as u8;
    report[6] = ((param >> 8) & 0xff) as u8;
    report[11] = (param & 0xff) as u8;
    report
}

fn byte3_report(mut report: [u8; 15], value: u8) -> [u8; 15] {
    report[3] = value;
    report
}

fn format_bytes(report: &[u8]) -> String {
    report
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn auto_device() -> Result<String> {
    let entries = fs::read_dir("/sys/class/hidraw")
        .map_err(|err| format!("failed to read hidraw sysfs: {err}"))?;
    let mut matches = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| format!("failed to read hidraw entry: {err}"))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let uevent = entry.path().join("device/uevent");
        let Ok(content) = fs::read_to_string(uevent) else {
            continue;
        };
        let supported = content
            .lines()
            .any(|line| line.eq_ignore_ascii_case("HID_ID=0005:0000004C:00000265"));
        if supported {
            matches.push(format!("/dev/{name}"));
        }
    }
    match matches.len() {
        0 => Err("could not find Bluetooth Magic Trackpad 2".to_string()),
        1 => Ok(matches.remove(0)),
        _ => Err(format!(
            "multiple Magic Trackpad 2 devices found: {}",
            matches.join(", ")
        )),
    }
}

fn should_fire_release(
    config: &Config,
    pressure: i32,
    previous_pressure: Option<i32>,
    previous_time: Option<Instant>,
    peak_after_down: i32,
    down_time: Option<Instant>,
    now: Instant,
) -> (bool, String) {
    if pressure <= config.clicks.reset_threshold {
        return (true, "threshold".to_string());
    }
    if let Some(down_time) = down_time {
        if now.duration_since(down_time).as_secs_f64() < config.release.lockout_ms / 1000.0 {
            return (false, String::new());
        }
    }
    if peak_after_down > 0 && peak_after_down - pressure >= config.release.drop {
        return (true, "drop".to_string());
    }
    if let (Some(previous_pressure), Some(previous_time)) = (previous_pressure, previous_time) {
        let elapsed = now.duration_since(previous_time).as_secs_f64();
        if elapsed > 0.0 {
            let rate = (previous_pressure - pressure) as f64 / elapsed;
            if rate >= config.release.rate {
                return (true, format!("rate={rate:.1}/s"));
            }
        }
    }
    (false, String::new())
}

#[derive(Default)]
struct DragState {
    previous_centroid: Option<(f64, f64)>,
    distance_since_haptic: f64,
    touch_count: usize,
    last_haptic: Option<Instant>,
}

impl DragState {
    fn distance_for(config: &Config, touch_count: usize) -> f64 {
        if touch_count == 2 {
            config
                .drag
                .two_finger_distance
                .unwrap_or(config.drag.distance)
        } else if touch_count == 3 {
            config
                .drag
                .three_finger_distance
                .unwrap_or(config.drag.distance)
        } else {
            config.drag.distance
        }
    }

    fn min_gap_for(config: &Config, touch_count: usize) -> f64 {
        if touch_count == 2 {
            config
                .drag
                .two_finger_min_gap_ms
                .unwrap_or(config.drag.min_gap_ms)
        } else if touch_count == 3 {
            config
                .drag
                .three_finger_min_gap_ms
                .unwrap_or(config.drag.min_gap_ms)
        } else {
            config.drag.min_gap_ms
        }
    }

    fn maybe_send(
        &mut self,
        file: &mut File,
        config: &Config,
        reports: &Reports,
        now: Instant,
        last_click_haptic: Option<Instant>,
        pressure: i32,
        centroid: Option<(f64, f64)>,
        touch_count: usize,
    ) -> Result<()> {
        if !config.drag.enabled {
            return Ok(());
        }
        let click_silence_elapsed = config.drag.click_silence_ms <= 0.0
            || last_click_haptic
                .map(|last| {
                    now.duration_since(last).as_secs_f64() >= config.drag.click_silence_ms / 1000.0
                })
                .unwrap_or(true);
        let touch_count_allowed = config.drag.mode == DragMode::Any
            || (config.drag.mode == DragMode::Scroll && (touch_count == 2 || touch_count == 3));
        let drag_allowed = centroid.is_some()
            && touch_count_allowed
            && click_silence_elapsed
            && config
                .drag
                .max_pressure
                .map(|max| pressure <= max)
                .unwrap_or(true);
        if !drag_allowed {
            self.previous_centroid = centroid;
            self.distance_since_haptic = 0.0;
            self.touch_count = touch_count;
            return Ok(());
        }
        let centroid = centroid.unwrap();
        if self.previous_centroid.is_none() || self.touch_count != touch_count {
            self.previous_centroid = Some(centroid);
            self.distance_since_haptic = 0.0;
            self.touch_count = touch_count;
            return Ok(());
        }

        let previous = self.previous_centroid.unwrap();
        let moved = ((centroid.0 - previous.0).powi(2) + (centroid.1 - previous.1).powi(2)).sqrt();
        self.previous_centroid = Some(centroid);
        if moved <= config.drag.motion_epsilon {
            return Ok(());
        }

        self.distance_since_haptic += moved;
        let distance = Self::distance_for(config, touch_count);
        let min_gap_ms = Self::min_gap_for(config, touch_count);
        let gap_elapsed = self
            .last_haptic
            .map(|last| now.duration_since(last).as_secs_f64() >= min_gap_ms / 1000.0)
            .unwrap_or(true);
        if self.distance_since_haptic < distance {
            return Ok(());
        }
        if !gap_elapsed {
            if config.drag.debug {
                println!(
                    "drag-skip pressure={pressure} fingers={touch_count} distance={:.1} threshold={} reason=min-gap",
                    self.distance_since_haptic,
                    format_number(distance)
                );
            }
            self.distance_since_haptic = 0.0;
            return Ok(());
        }

        if config.drag.debug {
            println!(
                "drag-haptic pressure={pressure} fingers={touch_count} distance={:.1} threshold={} min-gap={}ms",
                self.distance_since_haptic,
                format_number(distance),
                format_number(min_gap_ms)
            );
        }
        write_output(file, &reports.drag_down)?;
        thread::sleep(Duration::from_secs_f64(config.drag.down_ms / 1000.0));
        write_output(file, &reports.drag_up)?;
        self.last_haptic = Some(Instant::now());
        self.distance_since_haptic = (self.distance_since_haptic - distance).max(0.0);
        self.touch_count = touch_count;
        Ok(())
    }
}

#[derive(Default)]
struct RidgeState {
    previous_side: Option<i8>,
    previous_cell: Option<(i32, i32)>,
    last_haptic: Option<Instant>,
}

impl RidgeState {
    fn grid_enabled(config: &Config) -> bool {
        config.ridge.grid_columns > 1 || config.ridge.grid_rows > 1
    }

    fn side(config: &Config, centroid: (f64, f64)) -> Option<i8> {
        let half_deadband = config.ridge.deadband / 2.0;
        if centroid.0 < config.ridge.position - half_deadband {
            Some(-1)
        } else if centroid.0 > config.ridge.position + half_deadband {
            Some(1)
        } else {
            None
        }
    }

    fn grid_axis_index(value: f64, min: f64, max: f64, cells: i32, deadband: f64) -> Option<i32> {
        if cells <= 1 {
            return Some(0);
        }

        let span = max - min;
        let half_deadband = deadband / 2.0;
        for line in 1..cells {
            let position = min + span * f64::from(line) / f64::from(cells);
            if (value - position).abs() <= half_deadband {
                return None;
            }
        }

        let raw_index = ((value - min) / span * f64::from(cells)).floor() as i32;
        Some(raw_index.clamp(0, cells - 1))
    }

    fn grid_cell(config: &Config, centroid: (f64, f64)) -> Option<(i32, i32)> {
        let column = Self::grid_axis_index(
            centroid.0,
            config.ridge.grid_x_min,
            config.ridge.grid_x_max,
            config.ridge.grid_columns,
            config.ridge.deadband,
        )?;
        let row = Self::grid_axis_index(
            centroid.1,
            config.ridge.grid_y_min,
            config.ridge.grid_y_max,
            config.ridge.grid_rows,
            config.ridge.deadband,
        )?;
        Some((column, row))
    }

    fn send_pulses(&mut self, file: &mut File, config: &Config, reports: &Reports) -> Result<()> {
        for pulse in 0..config.ridge.pulses {
            write_output(file, &reports.ridge_down)?;
            thread::sleep(Duration::from_secs_f64(config.ridge.down_ms / 1000.0));
            write_output(file, &reports.ridge_up)?;
            if pulse + 1 < config.ridge.pulses {
                thread::sleep(Duration::from_secs_f64(config.ridge.pulse_gap_ms / 1000.0));
            }
        }
        self.last_haptic = Some(Instant::now());
        Ok(())
    }

    fn gap_elapsed(&self, config: &Config, now: Instant) -> bool {
        self.last_haptic
            .map(|last| now.duration_since(last).as_secs_f64() >= config.ridge.min_gap_ms / 1000.0)
            .unwrap_or(true)
    }

    fn maybe_send(
        &mut self,
        file: &mut File,
        config: &Config,
        reports: &Reports,
        now: Instant,
        pressure: i32,
        centroid: Option<(f64, f64)>,
        touch_count: usize,
    ) -> Result<()> {
        if !config.ridge.enabled {
            return Ok(());
        }
        let Some(centroid) = centroid else {
            self.previous_side = None;
            self.previous_cell = None;
            return Ok(());
        };

        if Self::grid_enabled(config) {
            return self.maybe_send_grid(
                file,
                config,
                reports,
                now,
                pressure,
                centroid,
                touch_count,
            );
        }

        self.maybe_send_single(file, config, reports, now, pressure, centroid, touch_count)
    }

    fn maybe_send_single(
        &mut self,
        file: &mut File,
        config: &Config,
        reports: &Reports,
        now: Instant,
        pressure: i32,
        centroid: (f64, f64),
        touch_count: usize,
    ) -> Result<()> {
        let Some(side) = Self::side(config, centroid) else {
            return Ok(());
        };
        let Some(previous_side) = self.previous_side else {
            self.previous_side = Some(side);
            return Ok(());
        };
        if previous_side == side {
            return Ok(());
        }

        self.previous_side = Some(side);
        if !self.gap_elapsed(config, now) {
            if config.ridge.debug {
                println!(
                    "ridge-skip pressure={pressure} fingers={touch_count} x={:.1} reason=min-gap",
                    centroid.0
                );
            }
            return Ok(());
        }

        if config.ridge.debug {
            println!(
                "ridge-haptic pressure={pressure} fingers={touch_count} x={:.1} position={} deadband={}",
                centroid.0,
                format_number(config.ridge.position),
                format_number(config.ridge.deadband)
            );
        }
        self.send_pulses(file, config, reports)
    }

    fn maybe_send_grid(
        &mut self,
        file: &mut File,
        config: &Config,
        reports: &Reports,
        now: Instant,
        pressure: i32,
        centroid: (f64, f64),
        touch_count: usize,
    ) -> Result<()> {
        let Some(cell) = Self::grid_cell(config, centroid) else {
            return Ok(());
        };
        let Some(previous_cell) = self.previous_cell else {
            self.previous_cell = Some(cell);
            return Ok(());
        };
        if previous_cell == cell {
            return Ok(());
        }

        self.previous_cell = Some(cell);
        if !self.gap_elapsed(config, now) {
            if config.ridge.debug {
                println!(
                    "ridge-grid-skip pressure={pressure} fingers={touch_count} x={:.1} y={:.1} cell={:?} reason=min-gap",
                    centroid.0, centroid.1, cell
                );
            }
            return Ok(());
        }

        if config.ridge.debug {
            println!(
                "ridge-grid-haptic pressure={pressure} fingers={touch_count} x={:.1} y={:.1} from={:?} to={:?}",
                centroid.0, centroid.1, previous_cell, cell
            );
        }
        self.send_pulses(file, config, reports)
    }
}

struct Reports {
    normal_down: [u8; 15],
    normal_up: [u8; 15],
    force_down: [u8; 15],
    force_up: [u8; 15],
    drag_down: [u8; 15],
    drag_up: [u8; 15],
    ridge_down: [u8; 15],
    ridge_up: [u8; 15],
}

impl Reports {
    fn from_config(config: &Config) -> Self {
        let base_down = haptic_report(VIB_DOWN_TEMPLATE, config.haptics.down_param);
        let base_up = haptic_report(VIB_UP_TEMPLATE, config.haptics.up_param);
        let normal_down = byte3_report(base_down, config.haptics.normal_down_byte3);
        let normal_up = byte3_report(base_up, config.haptics.normal_up_byte3);
        let force_down = byte3_report(base_down, config.haptics.force_down_byte3);
        let force_up = byte3_report(base_up, config.haptics.force_up_byte3);
        let ridge_down = byte3_report(
            haptic_report(VIB_DOWN_TEMPLATE, config.ridge.down_param),
            config.ridge.down_byte3,
        );
        let ridge_up = byte3_report(
            haptic_report(VIB_UP_TEMPLATE, config.ridge.up_param),
            config.ridge.up_byte3,
        );
        Self {
            normal_down,
            normal_up,
            force_down,
            force_up,
            drag_down: normal_down,
            drag_up: normal_up,
            ridge_down,
            ridge_up,
        }
    }
}

#[derive(Default)]
struct PressureState {
    stage: u8,
    normal_up_pending: bool,
    force_up_pending: bool,
    force_rearmed: bool,
    last_fire: Option<Instant>,
    last_force_release: Option<Instant>,
    last_click_haptic: Option<Instant>,
    last_normal_release: Option<Instant>,
    double_click_force_guard: bool,
    pending_normal_button: Option<u16>,
    pending_normal_deadline: Option<Instant>,
    pending_normal_centroid: Option<(f64, f64)>,
    normal_click_touch_count: usize,
    multi_touch_motion_last_centroid: Option<(f64, f64)>,
    multi_touch_motion_x_remainder: f64,
    multi_touch_motion_y_remainder: f64,
    click_drag_start_centroid: Option<(f64, f64)>,
    click_drag_active: bool,
    peak_after_down: i32,
    previous_pressure: Option<i32>,
    previous_time: Option<Instant>,
    drag: DragState,
    ridge: RidgeState,
}

fn send_click_down(file: &mut File, report: &[u8; 15], state: &mut PressureState) -> Result<()> {
    write_output(file, report)?;
    let now = Instant::now();
    state.last_fire = Some(now);
    state.last_click_haptic = Some(now);
    Ok(())
}

fn send_click_up(file: &mut File, report: &[u8; 15], state: &mut PressureState) -> Result<()> {
    write_output(file, report)?;
    state.last_click_haptic = Some(Instant::now());
    Ok(())
}

fn record_normal_release(state: &mut PressureState, now: Instant) {
    state.last_normal_release = Some(now);
    state.double_click_force_guard = false;
    state.normal_click_touch_count = 0;
    state.multi_touch_motion_last_centroid = None;
    state.multi_touch_motion_x_remainder = 0.0;
    state.multi_touch_motion_y_remainder = 0.0;
    state.click_drag_start_centroid = None;
    state.click_drag_active = false;
}

fn is_double_click_hold(
    config: &Config,
    state: &PressureState,
    now: Instant,
    touch_count: usize,
) -> bool {
    touch_count == 1
        && config.clicks.double_click_window_ms > 0.0
        && state
            .last_normal_release
            .map(|last| {
                now.duration_since(last).as_secs_f64()
                    <= config.clicks.double_click_window_ms / 1000.0
            })
            .unwrap_or(false)
}

fn active_force_threshold(config: &Config, state: &PressureState) -> i32 {
    if state.double_click_force_guard {
        config.clicks.double_click_force_threshold
    } else {
        config.clicks.force_threshold
    }
}

fn pending_normal_active(state: &PressureState) -> bool {
    state.pending_normal_button.is_some()
}

fn should_defer_normal_input(config: &Config, button: u16, touch_count: usize) -> bool {
    config.input.enabled
        && config.input.pending_left_ms > 0.0
        && button == BTN_LEFT
        && touch_count == 1
        && config.input.force_button.is_some()
        && config.input.force_button != Some(BTN_LEFT)
}

fn clear_pending_normal_input(state: &mut PressureState) {
    state.pending_normal_button = None;
    state.pending_normal_deadline = None;
    state.pending_normal_centroid = None;
}

fn arm_normal_input(
    config: &Config,
    mouse: &mut VirtualMouse,
    state: &mut PressureState,
    button: u16,
    touch_count: usize,
    centroid: Option<(f64, f64)>,
    now: Instant,
) -> Result<bool> {
    state.normal_click_touch_count = touch_count;
    state.multi_touch_motion_last_centroid = if touch_count >= 2 { centroid } else { None };
    state.multi_touch_motion_x_remainder = 0.0;
    state.multi_touch_motion_y_remainder = 0.0;
    if should_defer_normal_input(config, button, touch_count) {
        state.pending_normal_button = Some(button);
        state.pending_normal_deadline =
            Some(now + Duration::from_secs_f64(config.input.pending_left_ms / 1000.0));
        state.pending_normal_centroid = centroid;
        return Ok(true);
    }
    mouse.normal_down(button)?;
    Ok(false)
}

fn commit_pending_normal_input(
    mouse: &mut VirtualMouse,
    state: &mut PressureState,
    reason: &str,
) -> Result<()> {
    let Some(button) = state.pending_normal_button else {
        return Ok(());
    };
    println!("pending-left-down reason={reason}");
    clear_pending_normal_input(state);
    mouse.normal_down(button)
}

fn cancel_pending_normal_input(state: &mut PressureState, reason: &str) {
    if state.pending_normal_button.is_some() {
        println!("pending-left-cancel reason={reason}");
        clear_pending_normal_input(state);
    }
}

fn maybe_commit_pending_for_motion(
    config: &Config,
    mouse: &mut VirtualMouse,
    state: &mut PressureState,
    centroid: Option<(f64, f64)>,
) -> Result<()> {
    if state.pending_normal_button.is_none()
        || config.input.pending_left_motion <= 0.0
        || centroid.is_none()
        || state.pending_normal_centroid.is_none()
    {
        return Ok(());
    }
    let centroid = centroid.unwrap();
    let start = state.pending_normal_centroid.unwrap();
    let moved = ((centroid.0 - start.0).powi(2) + (centroid.1 - start.1).powi(2)).sqrt();
    if moved >= config.input.pending_left_motion {
        commit_pending_normal_input(mouse, state, &format!("motion={moved:.1}"))?;
    }
    Ok(())
}

fn maybe_send_multi_touch_drag_motion(
    config: &Config,
    mouse: &mut VirtualMouse,
    state: &mut PressureState,
    centroid: Option<(f64, f64)>,
    touch_count: usize,
) -> Result<()> {
    if !config.input.enabled
        || !config.input.multi_touch_drag_motion
        || !state.normal_up_pending
        || state.normal_click_touch_count < 2
    {
        return Ok(());
    }

    let Some(centroid) = centroid else {
        state.multi_touch_motion_last_centroid = None;
        state.multi_touch_motion_x_remainder = 0.0;
        state.multi_touch_motion_y_remainder = 0.0;
        return Ok(());
    };

    if touch_count != state.normal_click_touch_count {
        state.multi_touch_motion_last_centroid = Some(centroid);
        state.multi_touch_motion_x_remainder = 0.0;
        state.multi_touch_motion_y_remainder = 0.0;
        return Ok(());
    }

    let Some(last) = state.multi_touch_motion_last_centroid else {
        state.multi_touch_motion_last_centroid = Some(centroid);
        return Ok(());
    };

    state.multi_touch_motion_last_centroid = Some(centroid);
    let raw_dx = centroid.0 - last.0;
    let raw_dy = centroid.1 - last.1;
    let scaled_dx =
        raw_dx * config.input.multi_touch_motion_x_scale + state.multi_touch_motion_x_remainder;
    let scaled_dy =
        raw_dy * config.input.multi_touch_motion_y_scale + state.multi_touch_motion_y_remainder;
    let dx = scaled_dx.trunc() as i32;
    let dy = scaled_dy.trunc() as i32;
    state.multi_touch_motion_x_remainder = scaled_dx - f64::from(dx);
    state.multi_touch_motion_y_remainder = scaled_dy - f64::from(dy);

    if config.input.multi_touch_motion_debug && (dx != 0 || dy != 0) {
        println!(
            "multi-touch-motion fingers={touch_count} raw-dx={raw_dx:.1} raw-dy={raw_dy:.1} dx={dx} dy={dy}"
        );
    }

    mouse.rel_motion(dx, dy)
}

fn normal_release_threshold_for_current_click(config: &Config, state: &PressureState) -> i32 {
    if state.click_drag_active {
        config.drag_release.threshold
    } else {
        config.clicks.normal_release_threshold
    }
}

fn update_click_drag_state(
    config: &Config,
    state: &mut PressureState,
    centroid: Option<(f64, f64)>,
) {
    if !config.drag_release.enabled
        || state.click_drag_active
        || !state.normal_up_pending
        || state.stage != 1
    {
        return;
    }
    let (Some(start), Some(centroid)) = (state.click_drag_start_centroid, centroid) else {
        return;
    };
    let moved = ((centroid.0 - start.0).powi(2) + (centroid.1 - start.1).powi(2)).sqrt();
    if moved < config.drag_release.movement {
        return;
    }
    state.click_drag_active = true;
    if config.drag_release.debug {
        println!(
            "click-drag active moved={:.1} threshold={} release <= {}",
            moved,
            format_number(config.drag_release.movement),
            config.drag_release.threshold
        );
    }
}

fn maybe_send_motion_haptics(
    file: &mut File,
    mouse: &mut VirtualMouse,
    config: &Config,
    reports: &Reports,
    state: &mut PressureState,
    now: Instant,
    pressure: i32,
    centroid: Option<(f64, f64)>,
    touch_count: usize,
) -> Result<()> {
    maybe_send_multi_touch_drag_motion(config, mouse, state, centroid, touch_count)?;
    let last_click_haptic = state.last_click_haptic;
    state
        .ridge
        .maybe_send(file, config, reports, now, pressure, centroid, touch_count)?;
    state.drag.maybe_send(
        file,
        config,
        reports,
        now,
        last_click_haptic,
        pressure,
        centroid,
        touch_count,
    )
}

fn process_report(
    file: &mut File,
    mouse: &mut VirtualMouse,
    config: &Config,
    reports: &Reports,
    state: &mut PressureState,
    data: &[u8],
) -> Result<()> {
    let touches: Vec<Touch> = parse_touches(data)
        .into_iter()
        .filter(|touch| touch.down())
        .collect();
    let best_touch = touches.iter().max_by_key(|touch| touch.pressure).copied();
    let pressure = best_touch.map(|touch| touch.pressure).unwrap_or(0);
    let centroid = touch_centroid(&touches);
    let touch_count = touches.len();
    let now = Instant::now();
    let mut force_released_this_sample = false;

    update_click_drag_state(config, state, centroid);
    let (release, release_reason) = should_fire_release(
        config,
        pressure,
        state.previous_pressure,
        state.previous_time,
        state.peak_after_down,
        state.last_fire,
        now,
    );

    let normal_release_threshold = normal_release_threshold_for_current_click(config, state);
    let normal_release = pressure <= normal_release_threshold;

    if state.stage == 1 && state.normal_up_pending && normal_release {
        println!(
            "normal-release pressure={pressure} peak={} reason=threshold<={normal_release_threshold}",
            state.peak_after_down,
        );
        commit_pending_normal_input(mouse, state, "normal-release")?;
        mouse.normal_up()?;
        send_click_up(file, &reports.normal_up, state)?;
        state.normal_up_pending = false;
        state.stage = 4;
        record_normal_release(state, now);
    } else if state.stage == 2 && state.force_up_pending && release {
        println!(
            "force-release pressure={pressure} peak={} reason={release_reason}",
            state.peak_after_down
        );
        mouse.force_up()?;
        send_click_up(file, &reports.force_up, state)?;
        state.force_up_pending = false;
        state.stage = 3;
        force_released_this_sample = true;
        state.force_rearmed = false;
        state.last_force_release = Some(Instant::now());
    }

    if state.stage == 4 && pressure <= config.clicks.rearm_threshold {
        println!("re-armed at pressure={pressure} reason=normal-release");
        state.stage = 0;
        state.peak_after_down = 0;
        maybe_send_motion_haptics(
            file,
            mouse,
            config,
            reports,
            state,
            now,
            pressure,
            centroid,
            touch_count,
        )?;
        state.previous_pressure = Some(pressure);
        state.previous_time = Some(now);
        return Ok(());
    }

    if state.stage == 3 && state.normal_up_pending && normal_release {
        let suppress_normal_haptic = state
            .last_force_release
            .map(|last| {
                now.duration_since(last).as_secs_f64()
                    <= config.release.suppress_normal_up_after_force_ms / 1000.0
            })
            .unwrap_or(false);
        if force_released_this_sample
            && !suppress_normal_haptic
            && config.release.stacked_release_gap_ms > 0.0
        {
            thread::sleep(Duration::from_secs_f64(
                config.release.stacked_release_gap_ms / 1000.0,
            ));
        }
        println!(
            "normal-release pressure={pressure} reason=force-upper-release{}",
            if suppress_normal_haptic {
                " haptic=suppressed"
            } else {
                ""
            }
        );
        commit_pending_normal_input(mouse, state, "force-upper-release")?;
        mouse.normal_up()?;
        if !suppress_normal_haptic {
            send_click_up(file, &reports.normal_up, state)?;
        }
        state.normal_up_pending = false;
        record_normal_release(state, now);
        println!("re-armed at pressure={pressure} reason=force-upper-release");
        state.stage = 0;
        state.force_up_pending = false;
        state.force_rearmed = false;
        state.peak_after_down = 0;
        maybe_send_motion_haptics(
            file,
            mouse,
            config,
            reports,
            state,
            now,
            pressure,
            centroid,
            touch_count,
        )?;
        state.previous_pressure = Some(pressure);
        state.previous_time = Some(now);
        return Ok(());
    }

    if pressure <= config.clicks.reset_threshold {
        if state.stage == 2 && state.force_up_pending {
            println!(
                "force-release pressure={pressure} peak={} reason=reset",
                state.peak_after_down
            );
            mouse.force_up()?;
            send_click_up(file, &reports.force_up, state)?;
            state.force_up_pending = false;
            state.stage = 3;
            force_released_this_sample = true;
            state.force_rearmed = false;
            state.last_force_release = Some(Instant::now());
        }
        if state.normal_up_pending && normal_release {
            let suppress_normal_haptic = state
                .last_force_release
                .map(|last| {
                    Instant::now().duration_since(last).as_secs_f64()
                        <= config.release.suppress_normal_up_after_force_ms / 1000.0
                })
                .unwrap_or(false);
            if force_released_this_sample
                && !suppress_normal_haptic
                && config.release.stacked_release_gap_ms > 0.0
            {
                thread::sleep(Duration::from_secs_f64(
                    config.release.stacked_release_gap_ms / 1000.0,
                ));
            }
            println!(
                "normal-release pressure={pressure} reason=reset{}",
                if suppress_normal_haptic {
                    " haptic=suppressed"
                } else {
                    ""
                }
            );
            commit_pending_normal_input(mouse, state, "reset")?;
            mouse.normal_up()?;
            if !suppress_normal_haptic {
                send_click_up(file, &reports.normal_up, state)?;
            }
            state.normal_up_pending = false;
            record_normal_release(state, now);
        }
        if state.normal_up_pending {
            maybe_send_motion_haptics(
                file,
                mouse,
                config,
                reports,
                state,
                now,
                pressure,
                centroid,
                touch_count,
            )?;
            state.previous_pressure = Some(pressure);
            state.previous_time = Some(now);
            return Ok(());
        }
        if state.stage != 0 {
            println!("re-armed at pressure={pressure}");
        }
        state.stage = 0;
        state.force_up_pending = false;
        state.force_rearmed = false;
        state.peak_after_down = 0;
        maybe_send_motion_haptics(
            file,
            mouse,
            config,
            reports,
            state,
            now,
            pressure,
            centroid,
            touch_count,
        )?;
        state.previous_pressure = Some(pressure);
        state.previous_time = Some(now);
        return Ok(());
    }

    let location = best_touch
        .map(|touch| format!(" touch={} x={} y={}", touch.tracking_id, touch.x, touch.y))
        .unwrap_or_default();

    if state.stage == 0 && pressure >= config.clicks.normal_threshold {
        let normal_button = button_for_touch_count(touch_count);
        state.double_click_force_guard = is_double_click_hold(config, state, now, touch_count);
        let deferred_input = should_defer_normal_input(config, normal_button, touch_count);
        println!(
            "normal-click pressure={pressure} button={} fingers={touch_count} input={} force-threshold={}{}{location}",
            button_name(normal_button),
            if deferred_input { "pending" } else { "down" },
            active_force_threshold(config, state),
            if state.double_click_force_guard {
                " reason=double-click-guard"
            } else {
                ""
            }
        );
        arm_normal_input(
            config,
            mouse,
            state,
            normal_button,
            touch_count,
            centroid,
            now,
        )?;
        send_click_down(file, &reports.normal_down, state)?;
        state.stage = 1;
        state.normal_up_pending = true;
        state.force_up_pending = false;
        state.click_drag_start_centroid = if normal_button == BTN_LEFT {
            centroid
        } else {
            None
        };
        state.click_drag_active = false;
        state.peak_after_down = pressure;
        maybe_send_motion_haptics(
            file,
            mouse,
            config,
            reports,
            state,
            now,
            pressure,
            centroid,
            touch_count,
        )?;
        state.previous_pressure = Some(pressure);
        state.previous_time = Some(now);
        return Ok(());
    }

    let gap_elapsed = state
        .last_fire
        .map(|last| now.duration_since(last).as_secs_f64() >= config.clicks.min_gap_ms / 1000.0)
        .unwrap_or(true);
    let force_threshold = active_force_threshold(config, state);
    let force_gap_elapsed = gap_elapsed || pending_normal_active(state);
    if state.stage == 1 && force_gap_elapsed && pressure >= force_threshold {
        println!("force-click pressure={pressure} threshold={force_threshold}{location}");
        cancel_pending_normal_input(state, "force-click");
        mouse.force_down()?;
        send_click_down(file, &reports.force_down, state)?;
        state.stage = 2;
        state.force_up_pending = true;
        state.force_rearmed = false;
        state.peak_after_down = pressure;
    } else if state.stage == 3
        && !state.force_rearmed
        && pressure <= config.clicks.force_rearm_threshold
    {
        println!("force re-armed at pressure={pressure}");
        state.force_rearmed = true;
        state.peak_after_down = pressure;
    } else if state.stage == 3 && state.force_rearmed && gap_elapsed && pressure >= force_threshold
    {
        println!(
            "force-click pressure={pressure} threshold={force_threshold}{location} reason=re-force"
        );
        mouse.force_down()?;
        send_click_down(file, &reports.force_down, state)?;
        state.stage = 2;
        state.force_up_pending = true;
        state.force_rearmed = false;
        state.peak_after_down = pressure;
    } else if state.stage == 1 || state.stage == 2 {
        state.peak_after_down = state.peak_after_down.max(pressure);
    }

    if state.stage == 1 {
        maybe_commit_pending_for_motion(config, mouse, state, centroid)?;
    }

    maybe_send_motion_haptics(
        file,
        mouse,
        config,
        reports,
        state,
        now,
        pressure,
        centroid,
        touch_count,
    )?;
    state.previous_pressure = Some(pressure);
    state.previous_time = Some(now);
    Ok(())
}

fn timeout_millis_until(deadline: Instant, now: Instant) -> i32 {
    if deadline <= now {
        return 0;
    }
    deadline
        .duration_since(now)
        .as_millis()
        .min(c_int::MAX as u128) as c_int
}

fn next_read_timeout(end: Option<Instant>, state: &PressureState) -> c_int {
    let now = Instant::now();
    let mut timeout = 500;
    if let Some(end) = end {
        timeout = timeout.min(timeout_millis_until(end, now));
    }
    if let Some(deadline) = state.pending_normal_deadline {
        timeout = timeout.min(timeout_millis_until(deadline, now));
    }
    timeout
}

fn wait_readable(fd: RawFd, timeout_ms: c_int) -> Result<bool> {
    let mut poll_fd = PollFd {
        fd,
        events: POLLIN,
        revents: 0,
    };
    let result = unsafe { poll(&mut poll_fd, 1, timeout_ms) };
    if result > 0 {
        return Ok((poll_fd.revents & POLLIN) != 0);
    }
    if result == 0 {
        return Ok(false);
    }
    let err = io::Error::last_os_error();
    if err.kind() == ErrorKind::Interrupted {
        return Ok(false);
    }
    Err(format!("hidraw poll failed: {err}"))
}

fn run_daemon(config: Config, force: bool, seconds: Option<f64>) -> Result<()> {
    let device = config.device.path.clone().map_or_else(auto_device, Ok)?;
    let reports = Reports::from_config(&config);
    print_resolved_config(&config, &device);
    println!("normal down: {}", format_bytes(&reports.normal_down));
    println!("normal up:   {}", format_bytes(&reports.normal_up));
    println!("force down:  {}", format_bytes(&reports.force_down));
    println!("force up:    {}", format_bytes(&reports.force_up));

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&device)
        .map_err(|err| format!("failed to open {device}: {err}"))?;
    ensure_supported(&file, &device, force)?;
    set_feature_fd(file.as_raw_fd(), &HOST_CLICK_ON)?;
    let _guard = HostClickGuard {
        fd: file.as_raw_fd(),
        restore: config.device.restore_on_exit,
    };
    thread::sleep(Duration::from_secs_f64(config.device.settle_ms / 1000.0));
    println!("host-click mode enabled; both haptic stages are host-generated");

    let mut mouse = VirtualMouse::new(config.input.enabled, config.input.force_button)?;
    if config.input.enabled {
        println!(
            "virtual mouse enabled; {}",
            input_summary(config.input.force_button)
        );
    }
    println!("watching pressure; press Ctrl-C to stop");
    let end = seconds.map(|seconds| Instant::now() + Duration::from_secs_f64(seconds));
    let mut state = PressureState::default();
    let mut buf = [0_u8; 256];

    while RUNNING.load(Ordering::SeqCst) {
        if end.map(|end| Instant::now() >= end).unwrap_or(false) {
            break;
        }
        let timeout_ms = next_read_timeout(end, &state);
        let readable = wait_readable(file.as_raw_fd(), timeout_ms)?;
        if state
            .pending_normal_deadline
            .map(|deadline| Instant::now() >= deadline)
            .unwrap_or(false)
        {
            commit_pending_normal_input(&mut mouse, &mut state, "timeout")?;
        }
        if !readable {
            continue;
        }
        match file.read(&mut buf) {
            Ok(0) => continue,
            Ok(len) => process_report(
                &mut file,
                &mut mouse,
                &config,
                &reports,
                &mut state,
                &buf[..len],
            )?,
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => return Err(format!("hidraw read failed: {err}")),
        }
    }

    if state.force_up_pending {
        mouse.force_up()?;
        send_click_up(&mut file, &reports.force_up, &mut state)?;
    }
    if state.normal_up_pending {
        cancel_pending_normal_input(&mut state, "exit");
        mouse.normal_up()?;
        send_click_up(&mut file, &reports.normal_up, &mut state)?;
    }
    Ok(())
}

fn real_main() -> Result<()> {
    unsafe {
        signal(SIGINT, handle_signal);
        signal(SIGTERM, handle_signal);
    }

    let args = Args::parse()?;
    let mut config = Config::default();
    let config_path = args.config.clone().or_else(default_config_path);
    if let Some(path) = config_path.as_ref() {
        load_config_file(&mut config, path)?;
    }
    if let Some(device) = args.device {
        config.device.path = Some(device);
    }
    if args.no_input {
        config.input.enabled = false;
    }
    if let Some(force_button) = args.force_button {
        config.input.force_button = force_button;
    }
    if let Some(pending_left_ms) = args.pending_left_ms {
        config.input.pending_left_ms = pending_left_ms;
    }
    if let Some(pending_left_motion) = args.pending_left_motion {
        config.input.pending_left_motion = pending_left_motion;
    }
    if let Some(threshold) = args.double_click_force_threshold {
        config.clicks.double_click_force_threshold = threshold;
    }
    if let Some(window_ms) = args.double_click_window_ms {
        config.clicks.double_click_window_ms = window_ms;
    }
    if let Some(threshold) = args.normal_release_threshold {
        config.clicks.normal_release_threshold = threshold;
    }
    if let Some(threshold) = args.drag_release_threshold {
        config.drag_release.threshold = threshold;
    }
    if let Some(motion) = args.drag_release_motion {
        config.drag_release.movement = motion;
    }
    if args.no_restore {
        config.device.restore_on_exit = false;
    }
    validate_config(&config)?;

    let device = config
        .device
        .path
        .clone()
        .unwrap_or_else(|| "auto".to_string());
    if args.dry_run {
        let reports = Reports::from_config(&config);
        print_resolved_config(&config, &device);
        println!("normal down: {}", format_bytes(&reports.normal_down));
        println!("normal up:   {}", format_bytes(&reports.normal_up));
        println!("force down:  {}", format_bytes(&reports.force_down));
        println!("force up:    {}", format_bytes(&reports.force_up));
        println!("dry-run: no reports sent");
        return Ok(());
    }

    run_daemon(config, args.force, args.seconds)
}

fn main() -> ExitCode {
    match real_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
