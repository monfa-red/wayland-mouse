//! Configuration: layered TOML (`preset` → global overrides → per-device
//! overrides → DPI rescale) deserialized with serde, resolved into a flat
//! [`Settings`] the hot path uses.
//!
//! The file format is human-friendly (`[wheel] max_multiplier = 8.0`); the
//! runtime struct keeps the original mathematical field names. One serde type
//! tree is the single source of truth a future GUI can round-trip.

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Where the system config lives (a directory, so we can grow into it).
pub const CONFIG_DIR: &str = "/etc/wayland-mouse";
pub const CONFIG_PATH: &str = "/etc/wayland-mouse/config.toml";
/// Legacy v0.1 config, read only when migrating.
pub const OLD_CONFIG_PATH: &str = "/etc/scroll-accel.conf";

/// The `ptr_*` curve in every preset is expressed at this DPI; [`rescale`] maps
/// it to the device's actual DPI so the feel is DPI-independent (like macOS).
pub const REFERENCE_DPI: f64 = 1400.0;

/// Commented starter config written by `install` when none exists.
pub const DEFAULT_TEMPLATE: &str = include_str!("../wayland-mouse.toml.example");

// ---------------------------------------------------------------------------
// Runtime settings (resolved, per device — what the hot path reads)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Settings {
    // wheel acceleration
    pub wheel_enabled: bool,
    pub threshold_dps: f64,
    pub accel: f64,
    pub exponent: f64,
    pub max_mult: f64,
    pub attack: f64,
    pub release: f64,
    pub reset_gap: Duration,

    // pointer acceleration (macOS-like logistic S-curve)
    pub pointer_accel: bool,
    pub ptr_base: f64,
    pub ptr_max: f64,
    pub ptr_mid: f64,
    pub ptr_width: f64,
    pub ptr_tau: f64,
    pub dpi: f64,

    pub debug: bool,
}

// ---------------------------------------------------------------------------
// Presets — concrete curve sets, expressed at REFERENCE_DPI
// ---------------------------------------------------------------------------

/// Names accepted for the built-in presets (for validation / docs).
pub const PRESET_NAMES: &[&str] = &["mac-like", "subtle", "off"];

fn mac_like() -> Settings {
    Settings {
        wheel_enabled: true,
        threshold_dps: 8.0,
        accel: 0.1,
        exponent: 1.0,
        max_mult: 8.0,
        attack: 0.6,
        release: 0.15,
        reset_gap: Duration::from_millis(180),

        pointer_accel: true,
        ptr_base: 0.5,
        ptr_max: 2.5,
        ptr_mid: 4000.0,
        ptr_width: 2000.0,
        ptr_tau: 0.012,
        dpi: REFERENCE_DPI,

        debug: false,
    }
}

/// A gentle nudge rather than a transformation: kicks in later, ramps softer,
/// lower caps.
fn subtle() -> Settings {
    Settings {
        wheel_enabled: true,
        threshold_dps: 10.0,
        accel: 0.05,
        exponent: 1.0,
        max_mult: 4.0,
        attack: 0.5,
        release: 0.15,
        reset_gap: Duration::from_millis(180),

        pointer_accel: true,
        ptr_base: 0.8,
        ptr_max: 1.6,
        ptr_mid: 5000.0,
        ptr_width: 2500.0,
        ptr_tau: 0.012,
        dpi: REFERENCE_DPI,

        debug: false,
    }
}

/// Pure passthrough: both subsystems off, identity values if ever consulted.
fn off() -> Settings {
    Settings {
        wheel_enabled: false,
        threshold_dps: 0.0,
        accel: 0.0,
        exponent: 1.0,
        max_mult: 1.0,
        attack: 1.0,
        release: 1.0,
        reset_gap: Duration::from_millis(180),

        pointer_accel: false,
        ptr_base: 1.0,
        ptr_max: 1.0,
        ptr_mid: 4000.0,
        ptr_width: 2000.0,
        ptr_tau: 0.012,
        dpi: REFERENCE_DPI,

        debug: false,
    }
}

/// Resolve a preset by name; `None` if unknown (caller decides the fallback).
fn preset(name: &str) -> Option<Settings> {
    match name.trim().to_lowercase().as_str() {
        "mac-like" | "mac" | "macos" | "mac_like" => Some(mac_like()),
        "subtle" | "gentle" => Some(subtle()),
        "off" | "flat" | "none" | "passthrough" => Some(off()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// On-disk format (serde)
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(default)]
pub struct ConfigFile {
    pub preset: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dpi: Option<f64>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name_filter: String,
    #[serde(skip_serializing_if = "is_false")]
    pub debug: bool,
    #[serde(skip_serializing_if = "WheelCfg::is_empty")]
    pub wheel: WheelCfg,
    #[serde(skip_serializing_if = "PointerCfg::is_empty")]
    pub pointer: PointerCfg,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub device: Vec<DeviceRule>,
}

impl Default for ConfigFile {
    fn default() -> Self {
        ConfigFile {
            preset: "mac-like".to_string(),
            dpi: None,
            name_filter: String::new(),
            debug: false,
            wheel: WheelCfg::default(),
            pointer: PointerCfg::default(),
            device: Vec::new(),
        }
    }
}

#[derive(Deserialize, Serialize, Default, Clone, Debug)]
#[serde(default)]
pub struct WheelCfg {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strength: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub curve: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_multiplier: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smoothing_up: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smoothing_down: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_after_ms: Option<f64>,
}

impl WheelCfg {
    fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self.start_speed.is_none()
            && self.strength.is_none()
            && self.curve.is_none()
            && self.max_multiplier.is_none()
            && self.smoothing_up.is_none()
            && self.smoothing_down.is_none()
            && self.reset_after_ms.is_none()
    }
}

#[derive(Deserialize, Serialize, Default, Clone, Debug)]
#[serde(default)]
pub struct PointerCfg {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precision_gain: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_gain: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub midpoint_speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transition_width: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smoothing_ms: Option<f64>,
}

impl PointerCfg {
    fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self.precision_gain.is_none()
            && self.max_gain.is_none()
            && self.midpoint_speed.is_none()
            && self.transition_width.is_none()
            && self.smoothing_ms.is_none()
    }
}

#[derive(Deserialize, Serialize, Default, Clone, Debug)]
#[serde(default)]
pub struct DeviceRule {
    /// Case-insensitive substring matched against the device name.
    #[serde(rename = "match")]
    pub match_: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dpi: Option<f64>,
    #[serde(skip_serializing_if = "WheelCfg::is_empty")]
    pub wheel: WheelCfg,
    #[serde(skip_serializing_if = "PointerCfg::is_empty")]
    pub pointer: PointerCfg,
}

fn is_false(b: &bool) -> bool {
    !*b
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

impl ConfigFile {
    /// First device rule whose `match` substring is in `device_name`.
    fn rule_for<'a>(&'a self, device_name: &str) -> Option<&'a DeviceRule> {
        let lname = device_name.to_lowercase();
        self.device
            .iter()
            .find(|d| !d.match_.is_empty() && lname.contains(&d.match_.to_lowercase()))
    }

    /// Resolve the effective [`Settings`] for a device by name.
    ///
    /// Layering: base preset (device rule's preset if set, else the global
    /// preset) → global overrides → device overrides → DPI rescale.
    pub fn resolve(&self, device_name: &str) -> Settings {
        let rule = self.rule_for(device_name);
        let preset_name = rule
            .and_then(|r| r.preset.as_deref())
            .unwrap_or(&self.preset);
        let mut s = preset(preset_name).unwrap_or_else(|| {
            eprintln!("wayland-mouse: unknown preset {preset_name:?}, using 'mac-like'");
            mac_like()
        });

        apply_wheel(&mut s, &self.wheel);
        apply_pointer(&mut s, &self.pointer);
        if let Some(r) = rule {
            apply_wheel(&mut s, &r.wheel);
            apply_pointer(&mut s, &r.pointer);
        }

        s.dpi = rule
            .and_then(|r| r.dpi)
            .or(self.dpi)
            .unwrap_or(REFERENCE_DPI);
        s.debug = self.debug;
        rescale(&mut s);
        s
    }

    /// Effective settings with no device rule applied (the global baseline).
    pub fn resolve_global(&self) -> Settings {
        self.resolve("")
    }
}

fn apply_wheel(s: &mut Settings, w: &WheelCfg) {
    if let Some(v) = w.enabled {
        s.wheel_enabled = v;
    }
    if let Some(v) = w.start_speed {
        s.threshold_dps = v;
    }
    if let Some(v) = w.strength {
        s.accel = v;
    }
    if let Some(v) = w.curve {
        s.exponent = v;
    }
    if let Some(v) = w.max_multiplier {
        s.max_mult = v;
    }
    if let Some(v) = w.smoothing_up {
        s.attack = v;
    }
    if let Some(v) = w.smoothing_down {
        s.release = v;
    }
    if let Some(v) = w.reset_after_ms {
        s.reset_gap = Duration::from_secs_f64((v / 1000.0).max(0.0));
    }
}

fn apply_pointer(s: &mut Settings, p: &PointerCfg) {
    if let Some(v) = p.enabled {
        s.pointer_accel = v;
    }
    if let Some(v) = p.precision_gain {
        s.ptr_base = v;
    }
    if let Some(v) = p.max_gain {
        s.ptr_max = v;
    }
    if let Some(v) = p.midpoint_speed {
        s.ptr_mid = v;
    }
    if let Some(v) = p.transition_width {
        s.ptr_width = v;
    }
    if let Some(v) = p.smoothing_ms {
        s.ptr_tau = (v / 1000.0).max(0.0);
    }
}

/// Map the REFERENCE_DPI curve onto the device's real DPI: speed breakpoints
/// scale up, gains scale down, so the on-screen feel stays identical.
fn rescale(s: &mut Settings) {
    if s.dpi > 0.0 {
        let k = s.dpi / REFERENCE_DPI;
        s.ptr_mid *= k;
        s.ptr_width *= k;
        s.ptr_base /= k;
        s.ptr_max /= k;
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Load and parse the config. A missing file yields defaults (mac-like); only a
/// read or parse error is `Err`.
pub fn load(path: &Path) -> Result<ConfigFile, String> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ConfigFile::default()),
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    toml::from_str(&text).map_err(|e| format!("parsing {}:\n{e}", path.display()))
}

// ---------------------------------------------------------------------------
// Migration from the v0.1 flat key=value format
// ---------------------------------------------------------------------------

/// Translate a legacy `/etc/scroll-accel.conf` body into the new config.
pub fn migrate_old(old_text: &str) -> ConfigFile {
    let mut cf = ConfigFile::default();
    for line in old_text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        let v = v.split('#').next().unwrap_or("").trim();
        let f = |s: &str| s.parse::<f64>().ok();
        match k {
            "threshold_dps" => cf.wheel.start_speed = f(v),
            "accel" => cf.wheel.strength = f(v),
            "exponent" => cf.wheel.curve = f(v),
            "max_mult" => cf.wheel.max_multiplier = f(v),
            "attack" => cf.wheel.smoothing_up = f(v),
            "release" => cf.wheel.smoothing_down = f(v),
            "reset_gap_ms" => cf.wheel.reset_after_ms = f(v),
            "pointer_accel" => cf.pointer.enabled = Some(v != "0" && !v.is_empty()),
            "ptr_base_gain" => cf.pointer.precision_gain = f(v),
            "ptr_max_gain" => cf.pointer.max_gain = f(v),
            "ptr_mid_speed" => cf.pointer.midpoint_speed = f(v),
            "ptr_width" => cf.pointer.transition_width = f(v),
            "ptr_smoothing_ms" => cf.pointer.smoothing_ms = f(v),
            "mouse_dpi" => cf.dpi = f(v),
            "name_filter" => cf.name_filter = v.to_string(),
            "debug" => cf.debug = v != "0" && !v.is_empty(),
            _ => {}
        }
    }
    cf
}

/// Serialize a migrated config to TOML with a provenance header.
pub fn to_toml_string(cf: &ConfigFile) -> String {
    let body = toml::to_string_pretty(cf).unwrap_or_default();
    format!(
        "# wayland-mouse config — migrated from {OLD_CONFIG_PATH}.\n\
         # See https://github.com/monfa-red/wayland-mouse for all options and presets.\n\n{body}"
    )
}

// ---------------------------------------------------------------------------
// `config --print` and `config --check`
// ---------------------------------------------------------------------------

/// Print the effective (post-rescale) settings and any device rules.
pub fn print_effective(path: &Path) -> i32 {
    let cf = match load(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    if path.exists() {
        println!("# effective config from {}", path.display());
    } else {
        println!(
            "# no config at {} — built-in defaults (preset = mac-like)",
            path.display()
        );
    }
    print_settings("global", &cf.resolve_global());
    for r in &cf.device {
        if r.match_.is_empty() {
            continue;
        }
        println!();
        print_settings(
            &format!("device matching {:?}", r.match_),
            &cf.resolve(&r.match_),
        );
    }
    0
}

fn print_settings(label: &str, s: &Settings) {
    println!("[{label}]  dpi = {}", s.dpi);
    println!(
        "  wheel:   enabled={} start_speed={} strength={} curve={} max_multiplier={} \
         smoothing_up={} smoothing_down={} reset_after_ms={:.0}",
        s.wheel_enabled,
        s.threshold_dps,
        s.accel,
        s.exponent,
        s.max_mult,
        s.attack,
        s.release,
        s.reset_gap.as_secs_f64() * 1000.0,
    );
    println!(
        "  pointer: enabled={} precision_gain={:.3} max_gain={:.3} midpoint_speed={:.0} \
         transition_width={:.0} smoothing_ms={:.1}  (values shown after DPI rescale)",
        s.pointer_accel,
        s.ptr_base,
        s.ptr_max,
        s.ptr_mid,
        s.ptr_width,
        s.ptr_tau * 1000.0,
    );
}

/// Validate the config: syntax, unknown keys (warn), value ranges (warn),
/// unknown presets (warn). Returns a process exit code (non-zero only on a
/// hard parse error).
pub fn check(path: &Path) -> i32 {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!(
                "no config at {} — built-in defaults apply (preset = mac-like)",
                path.display()
            );
            return 0;
        }
        Err(e) => {
            eprintln!("error reading {}: {e}", path.display());
            return 1;
        }
    };

    // 1. Syntax + typed parse.
    let value: toml::Value = match toml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("✗ parse error in {}:\n{e}", path.display());
            return 1;
        }
    };
    let cf: ConfigFile = match toml::from_str(&text) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("✗ {}:\n{e}", path.display());
            return 1;
        }
    };

    let mut warnings = 0u32;
    let mut warn = |msg: String| {
        eprintln!("⚠ {msg}");
        warnings += 1;
    };

    // 2. Unknown keys.
    const TOP: &[&str] = &[
        "preset",
        "dpi",
        "name_filter",
        "debug",
        "wheel",
        "pointer",
        "device",
    ];
    const WHEEL: &[&str] = &[
        "enabled",
        "start_speed",
        "strength",
        "curve",
        "max_multiplier",
        "smoothing_up",
        "smoothing_down",
        "reset_after_ms",
    ];
    const POINTER: &[&str] = &[
        "enabled",
        "precision_gain",
        "max_gain",
        "midpoint_speed",
        "transition_width",
        "smoothing_ms",
    ];
    const DEVICE: &[&str] = &["match", "preset", "dpi", "wheel", "pointer"];

    if let Some(t) = value.as_table() {
        unknowns(t, TOP, "", &mut warn);
        if let Some(w) = t.get("wheel").and_then(|v| v.as_table()) {
            unknowns(w, WHEEL, "wheel.", &mut warn);
        }
        if let Some(p) = t.get("pointer").and_then(|v| v.as_table()) {
            unknowns(p, POINTER, "pointer.", &mut warn);
        }
        if let Some(arr) = t.get("device").and_then(|v| v.as_array()) {
            for (i, d) in arr.iter().enumerate() {
                if let Some(dt) = d.as_table() {
                    unknowns(dt, DEVICE, &format!("device[{i}]."), &mut warn);
                    if let Some(w) = dt.get("wheel").and_then(|v| v.as_table()) {
                        unknowns(w, WHEEL, &format!("device[{i}].wheel."), &mut warn);
                    }
                    if let Some(p) = dt.get("pointer").and_then(|v| v.as_table()) {
                        unknowns(p, POINTER, &format!("device[{i}].pointer."), &mut warn);
                    }
                }
            }
        }
    }

    // 3. Preset names.
    if preset(&cf.preset).is_none() {
        warn(format!(
            "preset {:?} is unknown (valid: {})",
            cf.preset,
            PRESET_NAMES.join(", ")
        ));
    }
    for r in &cf.device {
        if let Some(p) = &r.preset {
            if preset(p).is_none() {
                warn(format!("device {:?}: preset {:?} is unknown", r.match_, p));
            }
        }
    }

    // 4. Value ranges (on the resolved global settings).
    validate_ranges(&cf.resolve_global(), &mut warn);

    if warnings == 0 {
        println!("✓ {} is valid", path.display());
    } else {
        println!(
            "{} warning(s); config still loads (unknown keys/values are ignored or clamped)",
            warnings
        );
    }
    0
}

fn unknowns(table: &toml::Table, allowed: &[&str], prefix: &str, warn: &mut impl FnMut(String)) {
    for k in table.keys() {
        if !allowed.contains(&k.as_str()) {
            warn(format!("unknown key '{prefix}{k}'"));
        }
    }
}

fn validate_ranges(s: &Settings, warn: &mut impl FnMut(String)) {
    if s.dpi <= 0.0 {
        warn(format!("dpi must be > 0 (got {})", s.dpi));
    }
    if s.threshold_dps < 0.0 {
        warn("wheel.start_speed should be >= 0".into());
    }
    if s.accel < 0.0 {
        warn("wheel.strength should be >= 0".into());
    }
    if s.exponent <= 0.0 {
        warn("wheel.curve should be > 0".into());
    }
    if s.max_mult < 1.0 {
        warn("wheel.max_multiplier should be >= 1.0".into());
    }
    for (name, v) in [("smoothing_up", s.attack), ("smoothing_down", s.release)] {
        if v <= 0.0 || v > 1.0 {
            warn(format!("wheel.{name} should be in (0, 1] (got {v})"));
        }
    }
    if s.ptr_base <= 0.0 {
        warn("pointer.precision_gain should be > 0".into());
    }
    if s.ptr_max < s.ptr_base {
        warn("pointer.max_gain is below precision_gain (curve will invert)".into());
    }
    if s.ptr_mid <= 0.0 {
        warn("pointer.midpoint_speed should be > 0".into());
    }
    if s.ptr_width <= 0.0 {
        warn("pointer.transition_width should be > 0".into());
    }
    if s.ptr_tau <= 0.0 {
        warn("pointer.smoothing_ms should be > 0".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn default_is_mac_like() {
        let s = ConfigFile::default().resolve_global();
        assert!(s.wheel_enabled && s.pointer_accel);
        assert!(approx(s.threshold_dps, 8.0));
        assert!(approx(s.accel, 0.1));
        assert!(approx(s.max_mult, 8.0));
        // dpi == reference, so the curve is unscaled
        assert!(approx(s.ptr_base, 0.5));
        assert!(approx(s.ptr_max, 2.5));
        assert!(approx(s.ptr_mid, 4000.0));
        assert!(approx(s.dpi, 1400.0));
    }

    #[test]
    fn preset_off_disables_both() {
        let cf: ConfigFile = toml::from_str("preset = \"off\"").unwrap();
        let s = cf.resolve_global();
        assert!(!s.wheel_enabled);
        assert!(!s.pointer_accel);
    }

    #[test]
    fn global_override_layers_on_preset() {
        let cf: ConfigFile = toml::from_str(
            "preset = \"mac-like\"\n[pointer]\nmax_gain = 4.0\n[wheel]\nenabled = false\n",
        )
        .unwrap();
        let s = cf.resolve_global();
        assert!(approx(s.ptr_max, 4.0)); // overridden
        assert!(approx(s.ptr_base, 0.5)); // preset default kept
        assert!(!s.wheel_enabled); // overridden
    }

    #[test]
    fn dpi_rescale_keeps_feel() {
        // k = 2800/1400 = 2: speed breakpoints double, gains halve.
        let cf: ConfigFile = toml::from_str("preset = \"mac-like\"\ndpi = 2800\n").unwrap();
        let s = cf.resolve_global();
        assert!(approx(s.ptr_mid, 8000.0));
        assert!(approx(s.ptr_width, 4000.0));
        assert!(approx(s.ptr_base, 0.25));
        assert!(approx(s.ptr_max, 1.25));
    }

    #[test]
    fn per_device_rule_matches_and_overrides() {
        let cf: ConfigFile = toml::from_str(
            "preset = \"mac-like\"\n\
             [[device]]\nmatch = \"Logitech\"\npreset = \"off\"\n\
             [[device]]\nmatch = \"Trackball\"\n[device.pointer]\nenabled = false\n",
        )
        .unwrap();
        // non-matching device falls back to the global preset
        assert!(cf.resolve("Razer DeathAdder").pointer_accel);
        // "Logitech" → off preset
        let log = cf.resolve("Logitech USB Receiver Mouse");
        assert!(!log.wheel_enabled && !log.pointer_accel);
        // "Trackball" → mac-like base, pointer disabled by the device override
        let tb = cf.resolve("Kensington Trackball");
        assert!(tb.wheel_enabled && !tb.pointer_accel);
    }

    #[test]
    fn unknown_preset_falls_back_without_panicking() {
        let cf: ConfigFile = toml::from_str("preset = \"bogus\"").unwrap();
        let s = cf.resolve_global();
        assert!(approx(s.threshold_dps, 8.0)); // mac-like fallback
    }

    #[test]
    fn migration_preserves_values() {
        let old = "threshold_dps = 12\naccel = 0.2\nmax_mult = 6\n\
                   pointer_accel = 0\nptr_base_gain = 0.7\nptr_max_gain = 3.0\n\
                   ptr_mid_speed = 5000\nmouse_dpi = 1400\nname_filter = Keychron\n";
        let cf = migrate_old(old);
        assert_eq!(cf.name_filter, "Keychron");
        let s = cf.resolve_global();
        assert!(approx(s.threshold_dps, 12.0));
        assert!(approx(s.accel, 0.2));
        assert!(approx(s.max_mult, 6.0));
        assert!(!s.pointer_accel);
        assert!(approx(s.ptr_base, 0.7));
        assert!(approx(s.ptr_max, 3.0));
        assert!(approx(s.ptr_mid, 5000.0));
    }

    #[test]
    fn migrated_config_serializes_and_reparses() {
        let cf = migrate_old("threshold_dps = 9\nptr_max_gain = 2.0\nmouse_dpi = 800\n");
        let reparsed: ConfigFile = toml::from_str(&to_toml_string(&cf)).unwrap();
        let s = reparsed.resolve_global();
        assert!(approx(s.threshold_dps, 9.0));
        assert!(approx(s.dpi, 800.0));
    }

    #[test]
    fn shipped_example_is_valid() {
        let cf: ConfigFile = toml::from_str(DEFAULT_TEMPLATE).unwrap();
        let _ = cf.resolve_global();
        assert_eq!(cf.preset, "mac-like");
    }
}
