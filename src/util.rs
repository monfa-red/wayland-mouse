//! Small shared helpers.

use std::time::SystemTime;

/// Seconds between two kernel event timestamps; `+inf` if there is no previous
/// timestamp (first event), `0` if the clock ran backwards.
pub fn dt_secs(last: Option<SystemTime>, now: SystemTime) -> f64 {
    match last {
        Some(t) => now
            .duration_since(t)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0),
        None => f64::INFINITY,
    }
}
