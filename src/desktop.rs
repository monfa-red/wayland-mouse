//! Desktop integration: disabling the compositor's *own* pointer acceleration
//! so it doesn't stack a second curve on top of ours.
//!
//! This is the one desktop-specific piece. The mechanism differs per DE and
//! there's no portable runtime API, so we automate GNOME (and back it up for a
//! clean restore) and print exact instructions for everything else. Note this
//! is *advisory*: the daemon works without it, you just get double-accel — and
//! wheel-only users (`pointer.enabled = false`) need none of it.

use std::fs;
use std::process::Command;

use crate::config::CONFIG_DIR;

const GNOME_SCHEMA: &str = "org.gnome.desktop.peripherals.mouse";
pub const GNOME_BACKUP: &str = "/etc/wayland-mouse/gnome-accel.backup";

#[derive(Debug, Clone, PartialEq)]
pub enum Desktop {
    Gnome,
    Kde,
    Sway,
    Hyprland,
    Other(String),
    Unknown,
}

pub fn detect() -> Desktop {
    let xdg = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_lowercase();
    if xdg.contains("gnome") {
        Desktop::Gnome
    } else if xdg.contains("kde") || xdg.contains("plasma") {
        Desktop::Kde
    } else if xdg.contains("sway") {
        Desktop::Sway
    } else if xdg.contains("hyprland") {
        Desktop::Hyprland
    } else if xdg.is_empty() {
        Desktop::Unknown
    } else {
        Desktop::Other(xdg)
    }
}

/// The current GNOME pointer `(accel-profile, speed)` as the invoking user sees
/// them — for `status` to report whether acceleration is actually off. `None`
/// if not GNOME or gsettings is unavailable. Run this WITHOUT sudo so it reads
/// your own session.
pub fn gnome_accel_now() -> Option<(String, String)> {
    if detect() != Desktop::Gnome {
        return None;
    }
    let get = |key: &str| -> Option<String> {
        let out = Command::new("gsettings")
            .args(["get", GNOME_SCHEMA, key])
            .output()
            .ok()?;
        out.status.success().then(|| {
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .trim_matches('\'')
                .to_string()
        })
    };
    Some((get("accel-profile")?, get("speed")?))
}

/// The logged-in user behind `sudo` (gsettings is per-session, so we can't run
/// it as root).
fn sudo_user() -> Option<String> {
    std::env::var("SUDO_USER").ok().filter(|s| !s.is_empty())
}

fn user_uid(user: &str) -> Option<String> {
    let out = Command::new("id").arg("-u").arg(user).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// Run `gsettings <args>` as the logged-in user against their session bus.
fn run_user_gsettings(user: &str, args: &[&str]) -> Result<String, String> {
    let uid = user_uid(user).ok_or_else(|| format!("could not resolve uid for {user}"))?;
    let bus = format!("DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{uid}/bus");
    // Pass the bus address as a sudo command-line env assignment (matches the
    // env_reset-friendly form); .env() alone would be stripped by sudo.
    let out = Command::new("sudo")
        .arg("-u")
        .arg(user)
        .arg(bus)
        .arg("gsettings")
        .args(args)
        .output()
        .map_err(|e| format!("spawning gsettings: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Disable the compositor's native pointer accel. Returns `true` if it was
/// handled automatically; `false` means we printed manual instructions.
pub fn disable_native_accel() -> bool {
    match detect() {
        Desktop::Gnome => gnome_disable(),
        other => {
            print_manual(&other);
            false
        }
    }
}

fn gnome_disable() -> bool {
    let Some(user) = sudo_user() else {
        eprintln!("  note: run as your user to disable GNOME accel:");
        eprintln!("    gsettings set {GNOME_SCHEMA} accel-profile flat && gsettings set {GNOME_SCHEMA} speed 0.0");
        return false;
    };

    // Back up current values once.
    if !std::path::Path::new(GNOME_BACKUP).exists() {
        let profile =
            run_user_gsettings(&user, &["get", GNOME_SCHEMA, "accel-profile"]).unwrap_or_default();
        let speed = run_user_gsettings(&user, &["get", GNOME_SCHEMA, "speed"]).unwrap_or_default();
        let _ = fs::create_dir_all(CONFIG_DIR);
        let _ = fs::write(GNOME_BACKUP, format!("profile={profile}\nspeed={speed}\n"));
    }

    let ok = run_user_gsettings(&user, &["set", GNOME_SCHEMA, "accel-profile", "flat"]).is_ok()
        && run_user_gsettings(&user, &["set", GNOME_SCHEMA, "speed", "0.0"]).is_ok();
    if ok {
        eprintln!("  GNOME pointer accel set to flat (so only our curve applies)");
    } else {
        eprintln!("  note: couldn't set gsettings automatically. Run as your user:");
        eprintln!("    gsettings set {GNOME_SCHEMA} accel-profile flat && gsettings set {GNOME_SCHEMA} speed 0.0");
    }
    ok
}

/// Restore whatever native-accel settings we changed at install time.
pub fn restore_native_accel() {
    if detect() != Desktop::Gnome {
        return;
    }
    let Some(user) = sudo_user() else { return };
    let backup = GNOME_BACKUP;
    if !std::path::Path::new(backup).exists() {
        return;
    }
    let Ok(text) = fs::read_to_string(backup) else {
        return;
    };

    let mut profile = String::from("'default'");
    let mut speed = String::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("profile=") {
            if !v.is_empty() {
                profile = v.to_string();
            }
        } else if let Some(v) = line.strip_prefix("speed=") {
            speed = v.to_string();
        }
    }
    let _ = run_user_gsettings(&user, &["set", GNOME_SCHEMA, "accel-profile", &profile]);
    if !speed.is_empty() {
        let _ = run_user_gsettings(&user, &["set", GNOME_SCHEMA, "speed", &speed]);
    }
    let _ = fs::remove_file(backup);
    eprintln!("  restored GNOME mouse acceleration to its previous values");
}

fn print_manual(de: &Desktop) {
    eprintln!("  for best results, disable your compositor's own pointer accel:");
    match de {
        Desktop::Kde => {
            eprintln!("    KDE: System Settings → Mouse → set Acceleration Profile to 'Flat'");
            eprintln!("    (or per-device libinput settings in kcminputrc)");
        }
        Desktop::Sway => {
            eprintln!("    sway: in your config — input \"type:pointer\" {{ accel_profile flat; pointer_accel 0 }}");
        }
        Desktop::Hyprland => {
            eprintln!(
                "    Hyprland: in your config — input {{ accel_profile = flat; sensitivity = 0 }}"
            );
        }
        _ => {
            eprintln!(
                "    set your compositor's pointer acceleration profile to 'flat' / sensitivity 0."
            );
            eprintln!("    (not needed if you only use wheel accel: set pointer.enabled = false)");
        }
    }
}
