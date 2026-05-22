use std::env;
use std::ffi::c_int;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
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
const SYN_REPORT: u16 = 0x00;
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

static RUNNING: AtomicBool = AtomicBool::new(true);

unsafe extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    fn signal(signum: c_int, handler: extern "C" fn(c_int)) -> usize;
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
}

#[derive(Clone, Debug)]
struct ClickConfig {
    normal_threshold: i32,
    force_threshold: i32,
    reset_threshold: i32,
    rearm_threshold: i32,
    force_rearm_threshold: i32,
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
struct Config {
    device: DeviceConfig,
    input: InputConfig,
    clicks: ClickConfig,
    release: ReleaseConfig,
    haptics: HapticsConfig,
    drag: DragConfig,
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
            },
            clicks: ClickConfig {
                normal_threshold: 70,
                force_threshold: 165,
                reset_threshold: 35,
                rearm_threshold: 65,
                force_rearm_threshold: 125,
                min_gap_ms: 120.0,
            },
            release: ReleaseConfig {
                drop: 18,
                rate: 700.0,
                lockout_ms: 25.0,
                stacked_release_gap_ms: 35.0,
                suppress_normal_up_after_force_ms: 80.0,
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
           --no-input          Disable /dev/uinput mouse events\n\
           --no-restore        Leave host-click mode enabled on exit\n\
           --seconds N         Stop after N seconds\n"
    );
}

fn parse_f64(name: &str, value: &str) -> Result<f64> {
    value.parse::<f64>().map_err(|err| format!("{name}: {err}"))
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
        "clicks.normal_threshold" => config.clicks.normal_threshold = value_i32(key, value)?,
        "clicks.force_threshold" => config.clicks.force_threshold = value_i32(key, value)?,
        "clicks.reset_threshold" => config.clicks.reset_threshold = value_i32(key, value)?,
        "clicks.rearm_threshold" => config.clicks.rearm_threshold = value_i32(key, value)?,
        "clicks.force_rearm_threshold" => {
            config.clicks.force_rearm_threshold = value_i32(key, value)?;
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
        "thresholds: normal >= {}; force >= {}; force re-arm <= {}; re-arm <= {}; reset <= {}",
        config.clicks.normal_threshold,
        config.clicks.force_threshold,
        config.clicks.force_rearm_threshold,
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
        "input: {}",
        if config.input.enabled {
            input_summary(config.input.force_button)
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
            Some(Self::open_uinput()?)
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

    fn open_uinput() -> Result<File> {
        let mut file = OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .map_err(|err| format!("failed to open /dev/uinput: {err}"))?;
        let fd = file.as_raw_fd();
        ioctl_result(
            unsafe { ioctl(fd, ui_set_evbit(), EV_KEY as c_int) },
            "UI_SET_EVBIT",
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
            self.normal_up()?;
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

struct Reports {
    normal_down: [u8; 15],
    normal_up: [u8; 15],
    force_down: [u8; 15],
    force_up: [u8; 15],
    drag_down: [u8; 15],
    drag_up: [u8; 15],
}

impl Reports {
    fn from_config(config: &Config) -> Self {
        let base_down = haptic_report(VIB_DOWN_TEMPLATE, config.haptics.down_param);
        let base_up = haptic_report(VIB_UP_TEMPLATE, config.haptics.up_param);
        let normal_down = byte3_report(base_down, config.haptics.normal_down_byte3);
        let normal_up = byte3_report(base_up, config.haptics.normal_up_byte3);
        let force_down = byte3_report(base_down, config.haptics.force_down_byte3);
        let force_up = byte3_report(base_up, config.haptics.force_up_byte3);
        Self {
            normal_down,
            normal_up,
            force_down,
            force_up,
            drag_down: normal_down,
            drag_up: normal_up,
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
    peak_after_down: i32,
    previous_pressure: Option<i32>,
    previous_time: Option<Instant>,
    drag: DragState,
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

    let (release, release_reason) = should_fire_release(
        config,
        pressure,
        state.previous_pressure,
        state.previous_time,
        state.peak_after_down,
        state.last_fire,
        now,
    );

    if state.stage == 1 && state.normal_up_pending && release {
        println!(
            "normal-release pressure={pressure} peak={} reason={release_reason}",
            state.peak_after_down
        );
        mouse.normal_up()?;
        send_click_up(file, &reports.normal_up, state)?;
        state.normal_up_pending = false;
        state.stage = 4;
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
        state.drag.maybe_send(
            file,
            config,
            reports,
            now,
            state.last_click_haptic,
            pressure,
            centroid,
            touch_count,
        )?;
        state.previous_pressure = Some(pressure);
        state.previous_time = Some(now);
        return Ok(());
    }

    if state.stage == 3 && state.normal_up_pending && pressure <= config.clicks.rearm_threshold {
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
        mouse.normal_up()?;
        if !suppress_normal_haptic {
            send_click_up(file, &reports.normal_up, state)?;
        }
        state.normal_up_pending = false;
        println!("re-armed at pressure={pressure} reason=force-upper-release");
        state.stage = 0;
        state.force_up_pending = false;
        state.force_rearmed = false;
        state.peak_after_down = 0;
        state.drag.maybe_send(
            file,
            config,
            reports,
            now,
            state.last_click_haptic,
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
        if state.normal_up_pending {
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
            mouse.normal_up()?;
            if !suppress_normal_haptic {
                send_click_up(file, &reports.normal_up, state)?;
            }
            state.normal_up_pending = false;
        }
        if state.stage != 0 {
            println!("re-armed at pressure={pressure}");
        }
        state.stage = 0;
        state.force_up_pending = false;
        state.force_rearmed = false;
        state.peak_after_down = 0;
        state.drag.maybe_send(
            file,
            config,
            reports,
            now,
            state.last_click_haptic,
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
        println!(
            "normal-click pressure={pressure} button={} fingers={touch_count}{location}",
            button_name(normal_button)
        );
        mouse.normal_down(normal_button)?;
        send_click_down(file, &reports.normal_down, state)?;
        state.stage = 1;
        state.normal_up_pending = true;
        state.force_up_pending = false;
        state.peak_after_down = pressure;
        state.drag.maybe_send(
            file,
            config,
            reports,
            now,
            state.last_click_haptic,
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
    if state.stage == 1 && gap_elapsed && pressure >= config.clicks.force_threshold {
        println!("force-click pressure={pressure}{location}");
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
    } else if state.stage == 3
        && state.force_rearmed
        && gap_elapsed
        && pressure >= config.clicks.force_threshold
    {
        println!("force-click pressure={pressure}{location} reason=re-force");
        mouse.force_down()?;
        send_click_down(file, &reports.force_down, state)?;
        state.stage = 2;
        state.force_up_pending = true;
        state.force_rearmed = false;
        state.peak_after_down = pressure;
    } else if state.stage == 1 || state.stage == 2 {
        state.peak_after_down = state.peak_after_down.max(pressure);
    }

    state.drag.maybe_send(
        file,
        config,
        reports,
        now,
        state.last_click_haptic,
        pressure,
        centroid,
        touch_count,
    )?;
    state.previous_pressure = Some(pressure);
    state.previous_time = Some(now);
    Ok(())
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
