//! Daemon-side control plane: shared live state plus a tiny unix-socket server
//! the `tune` UI talks to. The hot path reads config through an `ArcSwap` (one
//! atomic load) and publishes telemetry through lock-free atomics, so live
//! tuning never stalls the input stream.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};

use crate::config::ConfigFile;
use crate::remap::{build_table, RemapTable, VirtualKeyboard};

/// Where the control socket lives. Root-owned (the daemon is a system service),
/// so `tune` connects as root too.
pub const SOCKET_PATH: &str = "/run/wayland-mouse.sock";

/// Latest measured values from the input stream, for the live curve markers.
/// Each f64 is stored as its bit pattern in an atomic.
#[derive(Default)]
pub struct Telemetry {
    pointer_speed: AtomicU64,
    pointer_gain: AtomicU64,
    wheel_dps: AtomicU64,
    wheel_mult: AtomicU64,
    /// evdev code of the most recently pressed button (0 = none/cleared). Used
    /// by the tuner's "press the button to map" capture flow.
    last_button: AtomicU32,
}

impl Telemetry {
    pub fn set_pointer(&self, speed: f64, gain: f64) {
        self.pointer_speed.store(speed.to_bits(), Ordering::Relaxed);
        self.pointer_gain.store(gain.to_bits(), Ordering::Relaxed);
    }
    pub fn set_wheel(&self, dps: f64, mult: f64) {
        self.wheel_dps.store(dps.to_bits(), Ordering::Relaxed);
        self.wheel_mult.store(mult.to_bits(), Ordering::Relaxed);
    }
    pub fn set_button(&self, code: u16) {
        self.last_button.store(code as u32, Ordering::Relaxed);
    }
    fn snapshot(&self) -> TelemetrySample {
        let load = |a: &AtomicU64| f64::from_bits(a.load(Ordering::Relaxed));
        TelemetrySample {
            pointer_speed: load(&self.pointer_speed),
            pointer_gain: load(&self.pointer_gain),
            wheel_dps: load(&self.wheel_dps),
            wheel_mult: load(&self.wheel_mult),
            last_button: self.last_button.load(Ordering::Relaxed),
        }
    }
}

/// State shared between the input threads and the control socket.
pub struct Shared {
    cfg: ArcSwap<ConfigFile>,
    version: AtomicU64,
    pub telemetry: Telemetry,
    config_path: PathBuf,
    /// Button → action map, rebuilt live on every config change.
    remap: ArcSwap<RemapTable>,
    /// The shared virtual keyboard (created once by the daemon; `None` if it
    /// couldn't be created, e.g. no uinput access).
    keyboard: Mutex<Option<Arc<VirtualKeyboard>>>,
}

impl Shared {
    pub fn new(cfg: ConfigFile, config_path: PathBuf) -> Arc<Self> {
        let remap = ArcSwap::from_pointee(build_table(&cfg.button));
        Arc::new(Shared {
            cfg: ArcSwap::from_pointee(cfg),
            version: AtomicU64::new(0),
            telemetry: Telemetry::default(),
            config_path,
            remap,
            keyboard: Mutex::new(None),
        })
    }

    /// The current config (cheap: one atomic load + refcount bump).
    pub fn current(&self) -> Arc<ConfigFile> {
        self.cfg.load_full()
    }

    /// A monotonically increasing counter the input threads watch to know when
    /// to re-resolve their settings.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    /// Replace the live config, rebuild the remap table, and bump the version.
    pub fn replace(&self, cfg: ConfigFile) {
        self.remap.store(Arc::new(build_table(&cfg.button)));
        self.cfg.store(Arc::new(cfg));
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    /// The current button remap table (cheap atomic load).
    pub fn remap(&self) -> Arc<RemapTable> {
        self.remap.load_full()
    }

    /// The shared virtual keyboard, created on first use. We only ever create
    /// it when a button remap actually needs it, so users who don't remap any
    /// buttons get no extra uinput device. Returns `None` only if the device
    /// couldn't be created (e.g. no uinput access).
    pub fn ensure_keyboard(&self) -> Option<Arc<VirtualKeyboard>> {
        let mut guard = self.keyboard.lock().unwrap();
        if guard.is_none() {
            match VirtualKeyboard::new_full() {
                Ok(kb) => *guard = Some(Arc::new(kb)),
                Err(e) => {
                    eprintln!("wayland-mouse: could not create virtual keyboard ({e}); button remap skipped");
                    return None;
                }
            }
        }
        guard.clone()
    }

    /// Persist the current live config to disk as TOML.
    fn save(&self) -> Result<(), String> {
        let cfg = self.current();
        let body = toml::to_string_pretty(&*cfg).map_err(|e| e.to_string())?;
        let text = format!(
            "# wayland-mouse config — written by `wayland-mouse tune`.\n\
             # Hand-editable; see https://github.com/monfa-red/wayland-mouse for all options.\n\n{body}"
        );
        std::fs::write(&self.config_path, text).map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Wire protocol (newline-delimited JSON, request/response)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    GetConfig,
    SetConfig {
        config: Box<ConfigFile>,
    },
    Save,
    GetTelemetry,
    /// Clear `last_button` so the next press is recognised as a fresh capture.
    ArmCapture,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Config { config: Box<ConfigFile> },
    Telemetry(TelemetrySample),
    Ok,
    Error { message: String },
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default)]
pub struct TelemetrySample {
    pub pointer_speed: f64,
    pub pointer_gain: f64,
    pub wheel_dps: f64,
    pub wheel_mult: f64,
    pub last_button: u32,
}

/// Spawn the control-socket server. Best-effort: a failure here just means live
/// tuning is unavailable; the daemon keeps accelerating.
pub fn serve(shared: Arc<Shared>) {
    let _ = std::fs::remove_file(SOCKET_PATH); // clear any stale socket
    let listener = match UnixListener::bind(SOCKET_PATH) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("wayland-mouse: control socket unavailable ({e}); live tuning disabled");
            return;
        }
    };
    let _ = std::fs::set_permissions(SOCKET_PATH, std::fs::Permissions::from_mode(0o660));

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let shared = shared.clone();
        thread::spawn(move || handle_client(stream, shared));
    }
}

fn handle_client(stream: UnixStream, shared: Arc<Shared>) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut writer = stream;
    let reader = BufReader::new(read_half);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(req) => process(req, &shared),
            Err(e) => Response::Error {
                message: format!("bad request: {e}"),
            },
        };
        let Ok(mut json) = serde_json::to_string(&resp) else {
            break;
        };
        json.push('\n');
        if writer.write_all(json.as_bytes()).is_err() {
            break;
        }
    }
}

fn process(req: Request, shared: &Shared) -> Response {
    match req {
        Request::GetConfig => Response::Config {
            config: Box::new((*shared.current()).clone()),
        },
        Request::SetConfig { config } => {
            shared.replace(*config);
            Response::Ok
        }
        Request::Save => match shared.save() {
            Ok(()) => Response::Ok,
            Err(message) => Response::Error { message },
        },
        Request::GetTelemetry => Response::Telemetry(shared.telemetry.snapshot()),
        Request::ArmCapture => {
            shared.telemetry.set_button(0);
            Response::Ok
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PointerCfg;
    use std::path::PathBuf;

    fn shared() -> Arc<Shared> {
        Shared::new(ConfigFile::default(), PathBuf::from("/dev/null"))
    }

    #[test]
    fn telemetry_roundtrip() {
        let s = shared();
        s.telemetry.set_pointer(1234.0, 1.8);
        s.telemetry.set_wheel(12.5, 3.0);
        let snap = s.telemetry.snapshot();
        assert_eq!(snap.pointer_speed, 1234.0);
        assert_eq!(snap.pointer_gain, 1.8);
        assert_eq!(snap.wheel_dps, 12.5);
        assert_eq!(snap.wheel_mult, 3.0);
    }

    #[test]
    fn replace_bumps_version() {
        let s = shared();
        let v0 = s.version();
        s.replace(ConfigFile {
            preset: "subtle".into(),
            ..Default::default()
        });
        assert_eq!(s.version(), v0 + 1);
        assert_eq!(s.current().preset, "subtle");
    }

    #[test]
    fn process_get_then_set_config() {
        let s = shared();
        match process(Request::GetConfig, &s) {
            Response::Config { config } => assert_eq!(config.preset, "mac-like"),
            _ => panic!("expected config"),
        }
        let cf = ConfigFile {
            preset: "off".into(),
            ..Default::default()
        };
        assert!(matches!(
            process(
                Request::SetConfig {
                    config: Box::new(cf)
                },
                &s
            ),
            Response::Ok
        ));
        assert_eq!(s.current().preset, "off");
    }

    #[test]
    fn process_get_telemetry() {
        let s = shared();
        s.telemetry.set_pointer(500.0, 1.2);
        match process(Request::GetTelemetry, &s) {
            Response::Telemetry(t) => assert_eq!(t.pointer_speed, 500.0),
            _ => panic!("expected telemetry"),
        }
    }

    #[test]
    fn save_writes_parseable_toml() {
        let path = std::env::temp_dir().join("wayland-mouse-ipc-save-test.toml");
        let _ = std::fs::remove_file(&path);
        let s = Shared::new(ConfigFile::default(), path.clone());
        s.replace(ConfigFile {
            preset: "subtle".into(),
            pointer: PointerCfg {
                max_gain: Some(3.3),
                ..Default::default()
            },
            ..Default::default()
        });
        s.save().expect("save should succeed");
        let parsed: ConfigFile = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.preset, "subtle");
        assert_eq!(parsed.pointer.max_gain, Some(3.3));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn set_config_rebuilds_remap_live() {
        let s = shared(); // no button rules initially
        let side = evdev::KeyCode::BTN_SIDE.code();
        assert!(s.remap().get(side).is_none());
        let cf = ConfigFile {
            button: vec![crate::config::ButtonRule {
                match_: "BTN_SIDE".into(),
                keys: vec!["Super".into(), "Page_Up".into()],
                mode: None,
            }],
            ..Default::default()
        };
        assert!(matches!(
            process(
                Request::SetConfig {
                    config: Box::new(cf)
                },
                &s
            ),
            Response::Ok
        ));
        // The remap table updated without a restart.
        assert!(s.remap().get(side).is_some());
    }

    // The client (tune) and server (daemon) are split processes; this pins the
    // JSON wire contract they share so a serde tag change can't silently break it.
    #[test]
    fn wire_format_roundtrips() {
        let req = Request::SetConfig {
            config: Box::new(ConfigFile {
                preset: "off".into(),
                ..Default::default()
            }),
        };
        let back: Request = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert!(matches!(back, Request::SetConfig { config } if config.preset == "off"));

        let resp = Response::Telemetry(TelemetrySample {
            pointer_speed: 7.0,
            ..Default::default()
        });
        let back: Response = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert!(matches!(back, Response::Telemetry(t) if t.pointer_speed == 7.0));

        // Tagged unit variants too.
        let back: Request = serde_json::from_str(r#"{"cmd":"get_config"}"#).unwrap();
        assert!(matches!(back, Request::GetConfig));
    }
}
