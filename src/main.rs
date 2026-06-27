//! scroll-accel — Mos-like mouse-wheel scroll acceleration + macOS-like pointer
//! acceleration for Wayland/libinput.
//!
//! Wayland/GNOME exposes no wheel-speed setting, libinput has no scroll
//! multiplier, and GNOME only offers flat/adaptive pointer accel (no custom
//! curve). The only way to get full control is to intercept the mouse below the
//! compositor. This daemon grabs each physical wheel-mouse and re-emits via a
//! virtual uinput device, applying:
//!   * wheel: a velocity-dependent multiplier (slow = 1:1, faster ramps up), and
//!   * pointer: a logistic S-curve gain (slow = 1:1 for precision, fast = big
//!     reach), like macOS.
//!
//! Written in Rust because these mice poll at up to 8000 Hz: grabbing funnels
//! EVERY motion event through this process, so forwarding must be cheap. Motion
//! is accumulated per frame and accelerated at SYN_REPORT using the kernel event
//! timestamps (accurate even when events arrive batched). The grab is fd-tied,
//! so a crash releases the mouse instantly — no lockup.
//!
//! IMPORTANT: when pointer accel is on, set GNOME's mouse accel-profile to
//! 'flat' (and speed to 0) so libinput doesn't add a second curve on top.
//!
//! Run with `--debug` to print live wheel/pointer speed for tuning.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use evdev::uinput::VirtualDeviceBuilder;
use evdev::{AttributeSet, Device, EventType, InputEvent, Key, RelativeAxisType};

const VIRT_PREFIX: &str = "scroll-accel";
const CONFIG_PATH: &str = "/etc/scroll-accel.conf";
// Kernel convention: 120 high-resolution units == one wheel detent ("click").
const HIRES_PER_DETENT: f64 = 120.0;

const REL_WHEEL_HI: RelativeAxisType = RelativeAxisType(0x0b);
const REL_HWHEEL_HI: RelativeAxisType = RelativeAxisType(0x0c);

// The ptr_* curve values in the config are calibrated at this DPI. The daemon
// rescales them to the configured `mouse_dpi` so the feel is DPI-independent
// (like macOS): change your hardware DPI, set mouse_dpi to match, curve adapts.
const REFERENCE_DPI: f64 = 1400.0;

#[derive(Clone)]
struct Config {
    // --- wheel acceleration ---
    threshold_dps: f64,
    accel: f64,
    exponent: f64,
    max_mult: f64,
    attack: f64,
    release: f64,
    reset_gap: Duration,

    // --- pointer acceleration (macOS-like S-curve) ---
    pointer_accel: bool,
    ptr_base: f64,  // gain at very low speed (1.0 = 1:1, precise)
    ptr_max: f64,   // gain cap at high speed
    ptr_mid: f64,   // knee speed (device counts/sec) where gain is halfway
    ptr_width: f64, // transition width (counts/sec); larger = gentler S
    ptr_tau: f64,   // speed smoothing time constant (seconds)
    mouse_dpi: f64, // your actual hardware DPI; curve auto-scales from REFERENCE_DPI

    name_filter: String,
    debug: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            threshold_dps: 8.0,
            accel: 0.12,
            exponent: 1.0,
            max_mult: 8.0,
            attack: 0.6,
            release: 0.15,
            reset_gap: Duration::from_millis(180),

            pointer_accel: true,
            ptr_base: 1.0,
            ptr_max: 4.0,
            ptr_mid: 2000.0,
            ptr_width: 1000.0,
            ptr_tau: 0.012,
            mouse_dpi: REFERENCE_DPI,

            name_filter: String::new(),
            debug: false,
        }
    }
}

fn load_config() -> Config {
    let mut c = Config::default();
    if let Ok(text) = fs::read_to_string(CONFIG_PATH) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let k = k.trim();
                // strip any inline `# comment` from the value before parsing
                let v = v.split('#').next().unwrap_or("").trim();
                match k {
                    "threshold_dps" => drop(v.parse().map(|x| c.threshold_dps = x)),
                    "accel" => drop(v.parse().map(|x| c.accel = x)),
                    "exponent" => drop(v.parse().map(|x| c.exponent = x)),
                    "max_mult" => drop(v.parse().map(|x| c.max_mult = x)),
                    "attack" => drop(v.parse().map(|x| c.attack = x)),
                    "release" => drop(v.parse().map(|x| c.release = x)),
                    "reset_gap_ms" => {
                        drop(v.parse::<f64>().map(|x| c.reset_gap = Duration::from_secs_f64(x / 1000.0)))
                    }
                    "pointer_accel" => c.pointer_accel = v != "0" && !v.is_empty(),
                    "ptr_base_gain" => drop(v.parse().map(|x| c.ptr_base = x)),
                    "ptr_max_gain" => drop(v.parse().map(|x| c.ptr_max = x)),
                    "ptr_mid_speed" => drop(v.parse().map(|x| c.ptr_mid = x)),
                    "ptr_width" => drop(v.parse().map(|x| c.ptr_width = x)),
                    "ptr_smoothing_ms" => drop(v.parse::<f64>().map(|x| c.ptr_tau = x / 1000.0)),
                    "mouse_dpi" => drop(v.parse().map(|x| c.mouse_dpi = x)),
                    "name_filter" => c.name_filter = v.to_string(),
                    "debug" => c.debug = v != "0" && !v.is_empty(),
                    _ => {}
                }
            }
        }
    }
    // Rescale the pointer curve from REFERENCE_DPI to the actual mouse DPI so the
    // feel stays identical across DPI changes: speed breakpoints * k, gains / k.
    if c.mouse_dpi > 0.0 {
        let k = c.mouse_dpi / REFERENCE_DPI;
        c.ptr_mid *= k;
        c.ptr_width *= k;
        c.ptr_base /= k;
        c.ptr_max /= k;
    }
    if std::env::args().any(|a| a == "--debug") {
        c.debug = true;
    }
    if c.debug {
        eprintln!(
            "config: mouse_dpi={} (ref {}) -> ptr_base={:.3} ptr_max={:.3} ptr_mid={:.0} ptr_width={:.0}",
            c.mouse_dpi, REFERENCE_DPI, c.ptr_base, c.ptr_max, c.ptr_mid, c.ptr_width
        );
    }
    c
}

fn dt_secs(last: Option<SystemTime>, now: SystemTime) -> f64 {
    match last {
        Some(t) => now.duration_since(t).map(|d| d.as_secs_f64()).unwrap_or(0.0),
        None => f64::INFINITY,
    }
}

// ---------------- wheel ----------------

struct Axis {
    last: Option<SystemTime>,
    smoothed: f64, // detents/sec
    carry: f64,
}
impl Axis {
    fn new() -> Self {
        Axis { last: None, smoothed: 0.0, carry: 0.0 }
    }
}

fn mult_for_speed(c: &Config, dps: f64) -> f64 {
    let over = dps - c.threshold_dps;
    if over <= 0.0 {
        return 1.0;
    }
    (1.0 + c.accel * over.powf(c.exponent)).min(c.max_mult)
}

fn scroll(
    c: &Config,
    ax: &mut Axis,
    hires_in: i32,
    ts: SystemTime,
    out_code: RelativeAxisType,
    out: &mut Vec<InputEvent>,
    label: char,
) {
    let dt = dt_secs(ax.last, ts);
    ax.last = Some(ts);

    let detents = (hires_in.abs() as f64) / HIRES_PER_DETENT;
    let inst = if dt > c.reset_gap.as_secs_f64() {
        ax.smoothed = 0.0;
        0.0
    } else if dt <= 0.0 {
        ax.smoothed
    } else {
        detents / dt
    };
    let a = if inst > ax.smoothed { c.attack } else { c.release };
    ax.smoothed += a * (inst - ax.smoothed);

    let mult = mult_for_speed(c, ax.smoothed);
    ax.carry += (hires_in as f64) * mult;
    let outv = ax.carry.trunc() as i32;
    ax.carry -= outv as f64;

    if c.debug {
        eprintln!("{label} in={hires_in:+5} dps={:6.1} mult={mult:4.2} out={outv:+6}", ax.smoothed);
    }
    if outv != 0 {
        out.push(InputEvent::new(EventType::RELATIVE, out_code.0, outv));
    }
}

// ---------------- pointer ----------------

struct Pointer {
    last: Option<SystemTime>,
    speed: f64, // smoothed combined speed, counts/sec
    carry_x: f64,
    carry_y: f64,
}
impl Pointer {
    fn new() -> Self {
        Pointer { last: None, speed: 0.0, carry_x: 0.0, carry_y: 0.0 }
    }
}

fn pointer_gain(c: &Config, v: f64) -> f64 {
    let s = 1.0 / (1.0 + (-(v - c.ptr_mid) / c.ptr_width).exp()); // logistic 0..1
    c.ptr_base + (c.ptr_max - c.ptr_base) * s
}

fn accel_pointer(c: &Config, p: &mut Pointer, dx: i32, dy: i32, ts: SystemTime, out: &mut Vec<InputEvent>) {
    let dt = dt_secs(p.last, ts);
    p.last = Some(ts);

    let dist = (((dx * dx + dy * dy) as f64).sqrt()).abs();
    let v_inst = if dt > 0.0 && dt.is_finite() { dist / dt } else { p.speed };
    // time-constant EMA: correct regardless of (variable, high) event rate
    let alpha = if dt.is_finite() && dt > 0.0 { 1.0 - (-dt / c.ptr_tau).exp() } else { 1.0 };
    p.speed += alpha * (v_inst - p.speed);

    let gain = pointer_gain(c, p.speed);
    p.carry_x += dx as f64 * gain;
    p.carry_y += dy as f64 * gain;
    let ox = p.carry_x.trunc() as i32;
    let oy = p.carry_y.trunc() as i32;
    p.carry_x -= ox as f64;
    p.carry_y -= oy as f64;

    if c.debug {
        eprintln!("P dx={dx:+4} dy={dy:+4} v={:7.0} gain={gain:4.2} -> {ox:+4},{oy:+4}", p.speed);
    }
    if ox != 0 {
        out.push(InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_X.0, ox));
    }
    if oy != 0 {
        out.push(InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_Y.0, oy));
    }
}

// ---------------- device handling ----------------

fn run_device(path: PathBuf, cfg: Arc<Config>, handled: Arc<Mutex<HashSet<PathBuf>>>) -> io::Result<()> {
    let mut dev = Device::open(&path)?;
    let name = dev.name().unwrap_or("mouse").to_string();
    let has_hires = dev.supported_relative_axes().map_or(false, |s| s.contains(REL_WHEEL_HI));

    let mut axes = AttributeSet::<RelativeAxisType>::new();
    if let Some(s) = dev.supported_relative_axes() {
        for a in s.iter() {
            axes.insert(a);
        }
    }
    for a in [
        RelativeAxisType::REL_X,
        RelativeAxisType::REL_Y,
        RelativeAxisType::REL_WHEEL,
        RelativeAxisType::REL_HWHEEL,
        REL_WHEEL_HI,
        REL_HWHEEL_HI,
    ] {
        axes.insert(a);
    }
    let mut keys = AttributeSet::<Key>::new();
    if let Some(s) = dev.supported_keys() {
        for k in s.iter() {
            keys.insert(k);
        }
    }

    let vname = format!("{VIRT_PREFIX} {name}");
    let mut vdev = VirtualDeviceBuilder::new()?
        .name(&vname)
        .with_relative_axes(&axes)?
        .with_keys(&keys)?
        .build()?;

    dev.grab()?;
    eprintln!("handling {path:?}  {name:?}  hi-res={has_hires}  pointer_accel={}", cfg.pointer_accel);

    let mut vy = Axis::new();
    let mut hx = Axis::new();
    let mut ptr = Pointer::new();
    let mut fdx = 0i32; // accumulated frame motion
    let mut fdy = 0i32;
    let pa = cfg.pointer_accel;
    let mut out: Vec<InputEvent> = Vec::with_capacity(64);

    loop {
        let events = match dev.fetch_events() {
            Ok(e) => e,
            Err(_) => break,
        };
        out.clear();
        for ev in events {
            let et = ev.event_type();
            if et == EventType::RELATIVE {
                let code = ev.code();
                if code == REL_WHEEL_HI.0 {
                    scroll(&cfg, &mut vy, ev.value(), ev.timestamp(), REL_WHEEL_HI, &mut out, 'V');
                } else if code == REL_HWHEEL_HI.0 {
                    scroll(&cfg, &mut hx, ev.value(), ev.timestamp(), REL_HWHEEL_HI, &mut out, 'H');
                } else if code == RelativeAxisType::REL_WHEEL.0 {
                    if !has_hires {
                        scroll(&cfg, &mut vy, ev.value() * 120, ev.timestamp(), REL_WHEEL_HI, &mut out, 'V');
                    }
                } else if code == RelativeAxisType::REL_HWHEEL.0 {
                    if !has_hires {
                        scroll(&cfg, &mut hx, ev.value() * 120, ev.timestamp(), REL_HWHEEL_HI, &mut out, 'H');
                    }
                } else if code == RelativeAxisType::REL_X.0 {
                    if pa {
                        fdx += ev.value();
                    } else {
                        out.push(InputEvent::new(EventType::RELATIVE, code, ev.value()));
                    }
                } else if code == RelativeAxisType::REL_Y.0 {
                    if pa {
                        fdy += ev.value();
                    } else {
                        out.push(InputEvent::new(EventType::RELATIVE, code, ev.value()));
                    }
                } else {
                    out.push(InputEvent::new(EventType::RELATIVE, code, ev.value()));
                }
            } else if et == EventType::SYNCHRONIZATION && ev.code() == 0 {
                // SYN_REPORT: accelerate this frame's motion, then emit the SYN
                if pa && (fdx != 0 || fdy != 0) {
                    accel_pointer(&cfg, &mut ptr, fdx, fdy, ev.timestamp(), &mut out);
                    fdx = 0;
                    fdy = 0;
                }
                out.push(InputEvent::new(et, ev.code(), ev.value()));
            } else {
                out.push(InputEvent::new(et, ev.code(), ev.value()));
            }
        }
        if !out.is_empty() && vdev.emit(&out).is_err() {
            break;
        }
    }

    let _ = dev.ungrab();
    if let Ok(mut s) = handled.lock() {
        s.remove(&path);
    }
    eprintln!("released {path:?}");
    Ok(())
}

fn is_wheel_mouse(dev: &Device, filter: &str) -> bool {
    let name = dev.name().unwrap_or("");
    if name.contains(VIRT_PREFIX) {
        return false;
    }
    let has_wheel = dev
        .supported_relative_axes()
        .map_or(false, |s| s.contains(RelativeAxisType::REL_WHEEL) || s.contains(REL_WHEEL_HI));
    if !has_wheel {
        return false;
    }
    filter.is_empty() || name.to_lowercase().contains(&filter.to_lowercase())
}

fn main() {
    let cfg = Arc::new(load_config());
    let handled: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
    eprintln!("scroll-accel started (pointer_accel={}, debug={})", cfg.pointer_accel, cfg.debug);

    loop {
        for (path, dev) in evdev::enumerate() {
            {
                let s = handled.lock().unwrap();
                if s.contains(&path) {
                    continue;
                }
            }
            if is_wheel_mouse(&dev, &cfg.name_filter) {
                drop(dev);
                handled.lock().unwrap().insert(path.clone());
                let cfg = cfg.clone();
                let handled = handled.clone();
                let p = path.clone();
                thread::spawn(move || {
                    if let Err(e) = run_device(p.clone(), cfg, handled.clone()) {
                        eprintln!("device {p:?} error: {e}");
                        if let Ok(mut s) = handled.lock() {
                            s.remove(&p);
                        }
                    }
                });
            }
        }
        thread::sleep(Duration::from_secs(3));
    }
}
