//! Device handling: discover wheel mice, grab each one, and re-emit its events
//! through a per-device virtual uinput device with acceleration applied.

use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use evdev::uinput::VirtualDeviceBuilder;
use evdev::{AttributeSet, Device, EventType, InputEvent, Key, RelativeAxisType};

use crate::config::ConfigFile;
use crate::pointer::{accel_pointer, Pointer};
use crate::wheel::{scroll, Axis};

/// Virtual-device name prefix for devices we create. `is_wheel_mouse` skips
/// anything carrying this (or the legacy prefix) so we never grab our own output.
pub const VIRT_PREFIX: &str = "wayland-mouse";
/// v0.1 prefix — still filtered so a stale `scroll-accel` virtual device left
/// over mid-migration isn't grabbed.
pub const OLD_VIRT_PREFIX: &str = "scroll-accel";

const REL_WHEEL_HI: RelativeAxisType = RelativeAxisType(0x0b);
const REL_HWHEEL_HI: RelativeAxisType = RelativeAxisType(0x0c);

/// Daemon entry point: enumerate, grab new wheel mice, and watch for hotplug.
pub fn run(cfg: Arc<ConfigFile>) {
    let handled: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
    eprintln!(
        "wayland-mouse started (preset={}, name_filter={:?}, debug={})",
        cfg.preset, cfg.name_filter, cfg.debug
    );

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

fn is_wheel_mouse(dev: &Device, filter: &str) -> bool {
    let name = dev.name().unwrap_or("");
    if name.contains(VIRT_PREFIX) || name.contains(OLD_VIRT_PREFIX) {
        return false;
    }
    let has_wheel = dev
        .supported_relative_axes()
        .is_some_and(|s| s.contains(RelativeAxisType::REL_WHEEL) || s.contains(REL_WHEEL_HI));
    if !has_wheel {
        return false;
    }
    filter.is_empty() || name.to_lowercase().contains(&filter.to_lowercase())
}

fn run_device(
    path: PathBuf,
    cfg: Arc<ConfigFile>,
    handled: Arc<Mutex<HashSet<PathBuf>>>,
) -> io::Result<()> {
    let mut dev = Device::open(&path)?;
    let name = dev.name().unwrap_or("mouse").to_string();
    // Resolve this device's effective settings once, up front.
    let cfg = cfg.resolve(&name);
    let has_hires = dev
        .supported_relative_axes()
        .is_some_and(|s| s.contains(REL_WHEEL_HI));

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
    eprintln!(
        "handling {path:?}  {name:?}  hi-res={has_hires}  wheel={}  pointer={}",
        cfg.wheel_enabled, cfg.pointer_accel
    );

    let mut vy = Axis::new();
    let mut hx = Axis::new();
    let mut ptr = Pointer::new();
    let mut fdx = 0i32; // accumulated frame motion
    let mut fdy = 0i32;
    let pa = cfg.pointer_accel;
    let we = cfg.wheel_enabled;
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
                    if we {
                        scroll(
                            &cfg,
                            &mut vy,
                            ev.value(),
                            ev.timestamp(),
                            REL_WHEEL_HI,
                            &mut out,
                            'V',
                        );
                    } else {
                        out.push(InputEvent::new(et, code, ev.value()));
                    }
                } else if code == REL_HWHEEL_HI.0 {
                    if we {
                        scroll(
                            &cfg,
                            &mut hx,
                            ev.value(),
                            ev.timestamp(),
                            REL_HWHEEL_HI,
                            &mut out,
                            'H',
                        );
                    } else {
                        out.push(InputEvent::new(et, code, ev.value()));
                    }
                } else if code == RelativeAxisType::REL_WHEEL.0 {
                    // Coarse wheel: only when there's no hi-res stream to carry it.
                    if !we {
                        out.push(InputEvent::new(et, code, ev.value()));
                    } else if !has_hires {
                        scroll(
                            &cfg,
                            &mut vy,
                            ev.value() * 120,
                            ev.timestamp(),
                            REL_WHEEL_HI,
                            &mut out,
                            'V',
                        );
                    }
                } else if code == RelativeAxisType::REL_HWHEEL.0 {
                    if !we {
                        out.push(InputEvent::new(et, code, ev.value()));
                    } else if !has_hires {
                        scroll(
                            &cfg,
                            &mut hx,
                            ev.value() * 120,
                            ev.timestamp(),
                            REL_HWHEEL_HI,
                            &mut out,
                            'H',
                        );
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
