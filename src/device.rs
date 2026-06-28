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

use crate::ipc::Shared;
use crate::pointer::{accel_pointer, Pointer};
use crate::remap::{build_table, RemapTable, VirtualKeyboard};
use crate::wheel::{scroll, Axis};

/// Virtual-device name prefix for devices we create. `is_wheel_mouse` skips
/// anything carrying this so we never grab our own output.
pub const VIRT_PREFIX: &str = "wayland-mouse";

const REL_WHEEL_HI: RelativeAxisType = RelativeAxisType(0x0b);
const REL_HWHEEL_HI: RelativeAxisType = RelativeAxisType(0x0c);

/// Daemon entry point: enumerate, grab new wheel mice, and watch for hotplug.
pub fn run(shared: Arc<Shared>) {
    let handled: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
    let cfg = shared.current();

    // One shared virtual keyboard for all button remaps (created only if needed).
    // Button rules are resolved at startup; editing them in the tuner saves to
    // disk and applies on the next restart.
    let remap = Arc::new(build_table(&cfg.button));
    let keyboard = if remap.is_empty() {
        None
    } else {
        match VirtualKeyboard::new(remap.keyset()) {
            Ok(k) => Some(Arc::new(k)),
            Err(e) => {
                eprintln!("wayland-mouse: could not create virtual keyboard ({e}); button remaps disabled");
                None
            }
        }
    };

    // Control socket for the live-tuning UI.
    {
        let shared = shared.clone();
        thread::spawn(move || crate::ipc::serve(shared));
    }

    let name_filter = cfg.name_filter.clone();
    eprintln!(
        "wayland-mouse started (preset={}, name_filter={:?}, buttons={}, debug={})",
        cfg.preset,
        name_filter,
        cfg.button.len(),
        cfg.debug
    );

    loop {
        for (path, dev) in evdev::enumerate() {
            {
                let s = handled.lock().unwrap();
                if s.contains(&path) {
                    continue;
                }
            }
            if is_wheel_mouse(&dev, &name_filter) {
                drop(dev);
                handled.lock().unwrap().insert(path.clone());
                let shared = shared.clone();
                let handled = handled.clone();
                let remap = remap.clone();
                let keyboard = keyboard.clone();
                let p = path.clone();
                thread::spawn(move || {
                    if let Err(e) = run_device(p.clone(), shared, remap, keyboard, handled.clone())
                    {
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
    if name.contains(VIRT_PREFIX) {
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
    shared: Arc<Shared>,
    remap: Arc<RemapTable>,
    keyboard: Option<Arc<VirtualKeyboard>>,
    handled: Arc<Mutex<HashSet<PathBuf>>>,
) -> io::Result<()> {
    let mut dev = Device::open(&path)?;
    let name = dev.name().unwrap_or("mouse").to_string();
    // Resolve this device's effective settings; re-resolved live when the
    // config version changes (see the loop below).
    let mut settings = shared.current().resolve(&name);
    let mut version = shared.version();
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
        settings.wheel_enabled, settings.pointer_accel
    );

    let mut vy = Axis::new();
    let mut hx = Axis::new();
    let mut ptr = Pointer::new();
    let mut fdx = 0i32; // accumulated frame motion
    let mut fdy = 0i32;
    let mut pa = settings.pointer_accel;
    let mut we = settings.wheel_enabled;
    let mut out: Vec<InputEvent> = Vec::with_capacity(64);

    loop {
        let events = match dev.fetch_events() {
            Ok(e) => e,
            Err(_) => break,
        };
        // Pick up live config edits pushed by the tuner.
        let v = shared.version();
        if v != version {
            settings = shared.current().resolve(&name);
            version = v;
            pa = settings.pointer_accel;
            we = settings.wheel_enabled;
        }
        out.clear();
        for ev in events {
            let et = ev.event_type();
            if et == EventType::RELATIVE {
                let code = ev.code();
                if code == REL_WHEEL_HI.0 {
                    if we {
                        scroll(
                            &settings,
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
                            &settings,
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
                            &settings,
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
                            &settings,
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
            } else if et == EventType::KEY {
                // Remapped button: swallow it and emit the combo on the virtual
                // keyboard. Unmapped buttons pass straight through. Either way,
                // report the press so the tuner's capture flow can see it.
                let code = ev.code();
                if ev.value() == 1 {
                    shared.telemetry.set_button(code);
                }
                match keyboard.as_ref().zip(remap.get(code)) {
                    Some((kb, action)) => kb.apply(action, ev.value()),
                    None => out.push(InputEvent::new(et, code, ev.value())),
                }
            } else if et == EventType::SYNCHRONIZATION && ev.code() == 0 {
                // SYN_REPORT: accelerate this frame's motion, then emit the SYN
                if pa && (fdx != 0 || fdy != 0) {
                    accel_pointer(&settings, &mut ptr, fdx, fdy, ev.timestamp(), &mut out);
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
        // Publish the latest measured speeds/gains for the tuner's live markers.
        shared.telemetry.set_pointer(ptr.speed(), ptr.gain());
        shared.telemetry.set_wheel(vy.dps(), vy.mult());
    }

    let _ = dev.ungrab();
    if let Ok(mut s) = handled.lock() {
        s.remove(&path);
    }
    eprintln!("released {path:?}");
    Ok(())
}

/// `buttons` subcommand: print the evdev name of each mouse button as you press
/// it, so you can fill in `[[button]]` rules. Reads without grabbing, so the
/// buttons keep working normally while you identify them.
pub fn watch_buttons() -> i32 {
    let mut threads = Vec::new();
    let mut count = 0usize;
    for (_path, dev) in evdev::enumerate() {
        if !is_mouse_like(&dev) {
            continue;
        }
        let name = dev.name().unwrap_or("mouse").to_string();
        count += 1;
        let mut dev = dev;
        threads.push(thread::spawn(move || {
            while let Ok(evs) = dev.fetch_events() {
                for ev in evs {
                    if ev.event_type() == EventType::KEY && ev.value() == 1 {
                        println!("{name}: {:?}  (code {})", Key(ev.code()), ev.code());
                    }
                }
            }
        }));
    }
    if count == 0 {
        eprintln!("no mouse-like devices found — are you root?  (sudo wayland-mouse buttons)");
        return 1;
    }
    eprintln!("watching {count} device(s) — press your mouse buttons (Ctrl-C to stop)");
    for t in threads {
        let _ = t.join();
    }
    0
}

fn is_mouse_like(dev: &Device) -> bool {
    let name = dev.name().unwrap_or("");
    if name.contains(VIRT_PREFIX) {
        return false;
    }
    // BTN_MOUSE..=BTN_TASK (0x110..=0x117) covers the usual mouse buttons.
    let has_btn = dev
        .supported_keys()
        .is_some_and(|k| (0x110..=0x117).any(|c| k.contains(Key(c))));
    let has_wheel = dev
        .supported_relative_axes()
        .is_some_and(|s| s.contains(RelativeAxisType::REL_WHEEL) || s.contains(REL_WHEEL_HI));
    has_btn || has_wheel
}
