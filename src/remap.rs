//! Button remapping: map mouse buttons to key combos, emitted on a single
//! shared virtual keyboard so the compositor handles them as normal global
//! shortcuts (no GNOME D-Bus). The keyboard is a uinput device; if the daemon
//! dies, the kernel releases any held keys, so a stuck modifier can't outlive us.

use std::collections::HashMap;
use std::io;
use std::sync::Mutex;

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode as Key};

use crate::config::ButtonRule;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// Press the combo and release it on button-down (discrete actions like a
    /// workspace switch). Button-up is ignored.
    Tap,
    /// Mirror the button: keys go down on button-down, up on button-up.
    Hold,
}

pub struct Action {
    pub keys: Vec<Key>,
    pub mode: Mode,
}

/// Compiled button → action map. Rebuilt live whenever the config changes.
pub struct RemapTable {
    map: HashMap<u16, Action>,
}

impl RemapTable {
    pub fn get(&self, code: u16) -> Option<&Action> {
        self.map.get(&code)
    }
}

/// Compile config rules into a [`RemapTable`], logging (and skipping) bad rules.
pub fn build_table(rules: &[ButtonRule]) -> RemapTable {
    let mut map = HashMap::new();
    for r in rules {
        let Some(btn) = parse_button(&r.match_) else {
            eprintln!(
                "wayland-mouse: skipping button rule — unknown button {:?}",
                r.match_
            );
            continue;
        };
        let mut keys = Vec::with_capacity(r.keys.len());
        let mut ok = true;
        for name in &r.keys {
            match parse_key(name) {
                Some(k) => keys.push(k),
                None => {
                    eprintln!(
                        "wayland-mouse: button {:?} — unknown key {:?}",
                        r.match_, name
                    );
                    ok = false;
                }
            }
        }
        if !ok || keys.is_empty() {
            eprintln!(
                "wayland-mouse: skipping button rule {:?} (no valid keys)",
                r.match_
            );
            continue;
        }
        let mode = match r.mode.as_deref() {
            Some("hold") => Mode::Hold,
            _ => Mode::Tap,
        };
        map.insert(btn.code(), Action { keys, mode });
    }
    RemapTable { map }
}

/// One shared virtual keyboard for the whole daemon.
pub struct VirtualKeyboard {
    dev: Mutex<VirtualDevice>,
}

impl VirtualKeyboard {
    /// Build a keyboard that can emit any standard key, so bindings added live
    /// in the tuner always work regardless of which keys they use.
    pub fn new_full() -> io::Result<Self> {
        let mut keys = AttributeSet::<Key>::new();
        for code in 1u16..=248 {
            keys.insert(Key(code)); // KEY_* range (BTN_* start at 0x100)
        }
        Self::new(&keys)
    }

    pub fn new(keys: &AttributeSet<Key>) -> io::Result<Self> {
        let dev = VirtualDevice::builder()?
            .name("wayland-mouse keyboard")
            .with_keys(keys)?
            .build()?;
        Ok(VirtualKeyboard {
            dev: Mutex::new(dev),
        })
    }

    fn emit(&self, evs: &[InputEvent]) {
        if let Ok(mut d) = self.dev.lock() {
            let _ = d.emit(evs); // emit() appends its own SYN_REPORT
        }
    }

    /// Press keys in order (modifiers first), in one frame.
    fn press(&self, keys: &[Key]) {
        let evs: Vec<_> = keys
            .iter()
            .map(|k| InputEvent::new(EventType::KEY.0, k.code(), 1))
            .collect();
        self.emit(&evs);
    }

    /// Release keys in reverse order (key before modifiers), in one frame.
    fn release(&self, keys: &[Key]) {
        let evs: Vec<_> = keys
            .iter()
            .rev()
            .map(|k| InputEvent::new(EventType::KEY.0, k.code(), 0))
            .collect();
        self.emit(&evs);
    }

    /// React to a physical button event (`value`: 1 = press, 0 = release,
    /// 2 = autorepeat) per the action's mode. Press and release are separate
    /// frames so global shortcuts latch reliably.
    pub fn apply(&self, action: &Action, value: i32) {
        match action.mode {
            Mode::Tap => {
                if value == 1 {
                    self.press(&action.keys);
                    self.release(&action.keys);
                }
            }
            Mode::Hold => {
                if value == 1 {
                    self.press(&action.keys);
                } else if value == 0 {
                    self.release(&action.keys);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Name parsing — friendly aliases layered over evdev's own FromStr
// ---------------------------------------------------------------------------

/// Parse a key name: a friendly alias ("Super", "Page_Up"), or a raw evdev name
/// ("KEY_PAGEUP"), or a bare key ("a", "F5") resolved to KEY_*.
pub fn parse_key(name: &str) -> Option<Key> {
    let n = name.trim();
    if let Some(k) = key_alias(n) {
        return Some(k);
    }
    if let Ok(k) = n.parse::<Key>() {
        return Some(k);
    }
    let upper = n.to_ascii_uppercase();
    if let Ok(k) = upper.parse::<Key>() {
        return Some(k);
    }
    format!("KEY_{upper}").parse::<Key>().ok()
}

fn key_alias(name: &str) -> Option<Key> {
    Some(match name.to_ascii_lowercase().as_str() {
        "super" | "meta" | "win" | "windows" | "cmd" | "command" | "mod4" => Key::KEY_LEFTMETA,
        "rsuper" | "rmeta" => Key::KEY_RIGHTMETA,
        "ctrl" | "control" | "lctrl" => Key::KEY_LEFTCTRL,
        "rctrl" => Key::KEY_RIGHTCTRL,
        "alt" | "lalt" | "option" => Key::KEY_LEFTALT,
        "altgr" | "ralt" => Key::KEY_RIGHTALT,
        "shift" | "lshift" => Key::KEY_LEFTSHIFT,
        "rshift" => Key::KEY_RIGHTSHIFT,
        "page_up" | "pageup" | "pgup" => Key::KEY_PAGEUP,
        "page_down" | "pagedown" | "pgdn" | "pgdown" => Key::KEY_PAGEDOWN,
        "enter" | "return" => Key::KEY_ENTER,
        "esc" | "escape" => Key::KEY_ESC,
        "tab" => Key::KEY_TAB,
        "space" => Key::KEY_SPACE,
        "backspace" => Key::KEY_BACKSPACE,
        "delete" | "del" => Key::KEY_DELETE,
        "home" => Key::KEY_HOME,
        "end" => Key::KEY_END,
        "left" => Key::KEY_LEFT,
        "right" => Key::KEY_RIGHT,
        "up" => Key::KEY_UP,
        "down" => Key::KEY_DOWN,
        _ => return None,
    })
}

/// Parse a mouse-button name: a friendly alias ("side", "forward"), or a raw
/// evdev name ("BTN_SIDE").
pub fn parse_button(name: &str) -> Option<Key> {
    let n = name.trim();
    let alias = match n.to_ascii_lowercase().as_str() {
        "side" => Some(Key::BTN_SIDE),
        "extra" => Some(Key::BTN_EXTRA),
        "forward" => Some(Key::BTN_FORWARD),
        "back" => Some(Key::BTN_BACK),
        "middle" => Some(Key::BTN_MIDDLE),
        "left" => Some(Key::BTN_LEFT),
        "right" => Some(Key::BTN_RIGHT),
        _ => None,
    };
    if alias.is_some() {
        return alias;
    }
    n.parse::<Key>()
        .ok()
        .or_else(|| n.to_ascii_uppercase().parse::<Key>().ok())
}

/// Validate button rules for `config --check` (warnings only).
pub fn validate_buttons(rules: &[ButtonRule], warn: &mut impl FnMut(String)) {
    for (i, r) in rules.iter().enumerate() {
        if parse_button(&r.match_).is_none() {
            warn(format!(
                "button[{i}]: unknown button {:?} (try BTN_SIDE/BTN_EXTRA/BTN_FORWARD/BTN_BACK or side/extra/forward/back)",
                r.match_
            ));
        }
        if r.keys.is_empty() {
            warn(format!("button[{i}] ({:?}): no keys to send", r.match_));
        }
        for k in &r.keys {
            if parse_key(k).is_none() {
                warn(format!("button[{i}] ({:?}): unknown key {:?}", r.match_, k));
            }
        }
        if let Some(m) = &r.mode {
            if m != "tap" && m != "hold" {
                warn(format!(
                    "button[{i}]: unknown mode {:?} (use \"tap\" or \"hold\")",
                    m
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_aliases_and_raw_names() {
        assert_eq!(parse_key("Super"), Some(Key::KEY_LEFTMETA));
        assert_eq!(parse_key("Page_Up"), Some(Key::KEY_PAGEUP));
        assert_eq!(parse_key("ctrl"), Some(Key::KEY_LEFTCTRL));
        assert_eq!(parse_key("KEY_PAGEDOWN"), Some(Key::KEY_PAGEDOWN));
        assert_eq!(parse_key("a"), Some(Key::KEY_A));
        assert_eq!(parse_key("F5"), Some(Key::KEY_F5));
        assert_eq!(parse_key("nonsense"), None);
    }

    #[test]
    fn button_aliases_and_raw_names() {
        assert_eq!(parse_button("side"), Some(Key::BTN_SIDE));
        assert_eq!(parse_button("BTN_EXTRA"), Some(Key::BTN_EXTRA));
        assert_eq!(parse_button("forward"), Some(Key::BTN_FORWARD));
        assert_eq!(parse_button("BTN_BOGUS"), None);
    }

    #[test]
    fn build_table_compiles_valid_rules_only() {
        let rules = vec![
            ButtonRule {
                match_: "BTN_SIDE".into(),
                keys: vec!["Super".into(), "Page_Up".into()],
                mode: None,
            },
            ButtonRule {
                match_: "BTN_EXTRA".into(),
                keys: vec!["Super".into(), "Page_Down".into()],
                mode: Some("hold".into()),
            },
            ButtonRule {
                match_: "BTN_BOGUS".into(),
                keys: vec!["x".into()],
                mode: None,
            },
            ButtonRule {
                match_: "BTN_BACK".into(),
                keys: vec![],
                mode: None,
            },
        ];
        let t = build_table(&rules);
        assert_eq!(t.map.len(), 2); // bogus button + empty-keys rule dropped
        let side = t.get(Key::BTN_SIDE.code()).unwrap();
        assert_eq!(side.mode, Mode::Tap);
        assert_eq!(side.keys, vec![Key::KEY_LEFTMETA, Key::KEY_PAGEUP]);
        assert_eq!(t.get(Key::BTN_EXTRA.code()).unwrap().mode, Mode::Hold);
    }
}
