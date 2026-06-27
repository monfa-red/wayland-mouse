//! Wheel acceleration: a smoothed, velocity-dependent multiplier on the
//! high-resolution scroll stream. Slow scrolling stays 1:1; faster scrolling
//! ramps up to `max_multiplier`.

use std::time::SystemTime;

use evdev::{EventType, InputEvent, RelativeAxisType};

use crate::config::Settings;
use crate::util::dt_secs;

/// Kernel convention: 120 high-resolution units == one wheel detent ("click").
pub const HIRES_PER_DETENT: f64 = 120.0;

/// Per-axis smoothing state (one for vertical, one for horizontal).
pub struct Axis {
    last: Option<SystemTime>,
    smoothed: f64, // detents/sec
    carry: f64,
}

impl Axis {
    pub fn new() -> Self {
        Axis {
            last: None,
            smoothed: 0.0,
            carry: 0.0,
        }
    }
}

fn mult_for_speed(c: &Settings, dps: f64) -> f64 {
    let over = dps - c.threshold_dps;
    if over <= 0.0 {
        return 1.0;
    }
    (1.0 + c.accel * over.powf(c.exponent)).min(c.max_mult)
}

/// Accelerate one hi-res wheel event and push the result onto `out`.
#[allow(clippy::too_many_arguments)]
pub fn scroll(
    c: &Settings,
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
    let a = if inst > ax.smoothed {
        c.attack
    } else {
        c.release
    };
    ax.smoothed += a * (inst - ax.smoothed);

    let mult = mult_for_speed(c, ax.smoothed);
    ax.carry += (hires_in as f64) * mult;
    let outv = ax.carry.trunc() as i32;
    ax.carry -= outv as f64;

    if c.debug {
        eprintln!(
            "{label} in={hires_in:+5} dps={:6.1} mult={mult:4.2} out={outv:+6}",
            ax.smoothed
        );
    }
    if outv != 0 {
        out.push(InputEvent::new(EventType::RELATIVE, out_code.0, outv));
    }
}
