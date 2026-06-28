//! Pointer acceleration: a macOS-like logistic S-curve gain over the smoothed
//! cursor speed. Slow = 1:1 (precise); fast = a soft-capped reach multiplier.

use std::time::SystemTime;

use evdev::{EventType, InputEvent, RelativeAxisCode as RelativeAxisType};

use crate::config::Settings;
use crate::util::dt_secs;

/// Smoothed pointer state plus sub-count remainders carried between frames.
pub struct Pointer {
    last: Option<SystemTime>,
    speed: f64, // smoothed combined speed, counts/sec
    carry_x: f64,
    carry_y: f64,
    last_gain: f64,
}

impl Pointer {
    pub fn new() -> Self {
        Pointer {
            last: None,
            speed: 0.0,
            carry_x: 0.0,
            carry_y: 0.0,
            last_gain: 1.0,
        }
    }
    /// Latest smoothed pointer speed (counts/sec), for telemetry.
    pub fn speed(&self) -> f64 {
        self.speed
    }
    /// Latest applied gain, for telemetry.
    pub fn gain(&self) -> f64 {
        self.last_gain
    }
}

fn pointer_gain(c: &Settings, v: f64) -> f64 {
    let s = 1.0 / (1.0 + (-(v - c.ptr_mid) / c.ptr_width).exp()); // logistic 0..1
    c.ptr_base + (c.ptr_max - c.ptr_base) * s
}

/// Accelerate one frame's accumulated motion `(dx, dy)` and push the result.
pub fn accel_pointer(
    c: &Settings,
    p: &mut Pointer,
    dx: i32,
    dy: i32,
    ts: SystemTime,
    out: &mut Vec<InputEvent>,
) {
    let dt = dt_secs(p.last, ts);
    p.last = Some(ts);

    let dist = (((dx * dx + dy * dy) as f64).sqrt()).abs();
    let v_inst = if dt > 0.0 && dt.is_finite() {
        dist / dt
    } else {
        p.speed
    };
    // time-constant EMA: correct regardless of (variable, high) event rate
    let alpha = if dt.is_finite() && dt > 0.0 {
        1.0 - (-dt / c.ptr_tau).exp()
    } else {
        1.0
    };
    p.speed += alpha * (v_inst - p.speed);

    let gain = pointer_gain(c, p.speed);
    p.last_gain = gain;
    p.carry_x += dx as f64 * gain;
    p.carry_y += dy as f64 * gain;
    let ox = p.carry_x.trunc() as i32;
    let oy = p.carry_y.trunc() as i32;
    p.carry_x -= ox as f64;
    p.carry_y -= oy as f64;

    if c.debug {
        eprintln!(
            "P dx={dx:+4} dy={dy:+4} v={:7.0} gain={gain:4.2} -> {ox:+4},{oy:+4}",
            p.speed
        );
    }
    if ox != 0 {
        out.push(InputEvent::new(
            EventType::RELATIVE.0,
            RelativeAxisType::REL_X.0,
            ox,
        ));
    }
    if oy != 0 {
        out.push(InputEvent::new(
            EventType::RELATIVE.0,
            RelativeAxisType::REL_Y.0,
            oy,
        ));
    }
}
