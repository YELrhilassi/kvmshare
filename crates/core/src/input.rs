//! User input preferences: how forwarded motion and wheel feel on the
//! machines being controlled.
//!
//! Like the action engine, these live in the session (not the platform
//! layer) so they apply identically to every platform pair: the
//! transforms run on the *shared* input stream before it reaches any
//! client, whatever OS either side runs.
//!
//! * `pointer_speed` multiplies the server's measured pointer gain for
//!   forwarded motion — above 1 travels further per hand movement,
//!   below 1 travels less. It clamps to the same range as the measured
//!   gain itself, so extreme values can never desync the closed-loop
//!   position correction.
//! * `wheel_speed` multiplies forwarded wheel deltas (per-notch feel).
//! * `swap_scroll` inverts the vertical wheel direction — "natural
//!   scrolling" for people who want content to follow their fingers.

use serde::{Deserialize, Serialize};

/// The `[input]` config section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct InputPrefs {
    /// Multiplier on forwarded pointer motion (0.25–3.0).
    pub pointer_speed: f64,
    /// Multiplier on forwarded wheel deltas (0.25–8.0).
    pub wheel_speed: f64,
    /// Invert the vertical wheel direction (natural scrolling).
    pub swap_scroll: bool,
}

impl Default for InputPrefs {
    fn default() -> Self {
        Self { pointer_speed: 1.0, wheel_speed: 1.0, swap_scroll: false }
    }
}

impl InputPrefs {
    pub fn clamp(&mut self) {
        self.pointer_speed = self.pointer_speed.clamp(0.25, 3.0);
        self.wheel_speed = self.wheel_speed.clamp(0.25, 8.0);
    }

    /// Transform a wheel event per the preferences.
    pub fn transform_wheel(&self, dx: i32, dy: i32) -> (i32, i32) {
        let dy = if self.swap_scroll { -dy } else { dy };
        let s = self.wheel_speed;
        // Scale with rounding (f64 → nearest int): a 0.5 speed turns
        // every second notch into one line, exactly as intended.
        let dx = (dx as f64 * s).round() as i32;
        let dy = (dy as f64 * s).round() as i32;
        (dx, dy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_speed_scales_with_rounding() {
        let p = InputPrefs { wheel_speed: 0.5, ..Default::default() };
        assert_eq!(p.transform_wheel(0, 1), (0, 1)); // rounds to nearest
        assert_eq!(p.transform_wheel(0, 3), (0, 2));
        let p = InputPrefs { wheel_speed: 2.0, ..Default::default() };
        assert_eq!(p.transform_wheel(0, 3), (0, 6));
    }

    #[test]
    fn swap_scroll_inverts_vertical_only() {
        let p = InputPrefs { swap_scroll: true, ..Default::default() };
        assert_eq!(p.transform_wheel(2, -3), (2, 3));
    }

    #[test]
    fn clamp_bounds_extremes() {
        let mut p = InputPrefs { pointer_speed: 99.0, wheel_speed: 0.0, swap_scroll: false };
        p.clamp();
        assert_eq!(p.pointer_speed, 3.0);
        assert_eq!(p.wheel_speed, 0.25);
    }

    #[test]
    fn section_serializes_round_trip() {
        let s = toml::to_string(&InputPrefs::default()).unwrap();
        let back: InputPrefs = toml::from_str(&s).unwrap();
        assert_eq!(back, InputPrefs::default());
    }
}
