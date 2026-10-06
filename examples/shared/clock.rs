//! The `--time` option shared by the examples: the clock hour the world starts at
//! (`boxel::world::TimeOfDay`), and the light keys of `voxel_world`.
#![allow(
    dead_code,
    reason = "each example uses its own part (stress has no keys)"
)]

use boxel::world::TimeOfDay;

/// Help line of `--time` (two-space indent, flag column of 20).
pub const USAGE: &str =
    "  --time HH[:MM]      clock time the world starts at (06:00 sunrise, 12:00 noon, 18:00
                      sunset, 00:00 midnight; a day lasts 20 minutes); default 12:00";

/// Parses `HH` or `HH:MM` (00:00 to 23:59) into clock hours.
pub fn parse_clock(flag: &str, value: &str) -> Result<f64, String> {
    let bad = || format!("{flag}: {value:?} is not HH or HH:MM (00:00 to 23:59)");
    let (h, m) = value.split_once(':').unwrap_or((value, "0"));
    let h: u32 = h.trim().parse().map_err(|_| bad())?;
    let m: u32 = m.trim().parse().map_err(|_| bad())?;
    if h > 23 || m > 59 {
        return Err(bad());
    }
    Ok(f64::from(h) + f64::from(m) / 60.0)
}

/// The world clock at `hours`, paused when `paused` (scripted runs keep the light still).
pub fn start_time(hours: f64, paused: bool) -> TimeOfDay {
    TimeOfDay {
        paused,
        ..TimeOfDay::at_clock(hours)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_values() {
        assert_eq!(parse_clock("--time", "12"), Ok(12.0));
        assert_eq!(parse_clock("--time", "06:30"), Ok(6.5));
        assert_eq!(parse_clock("--time", "0:00"), Ok(0.0));
        for bad in ["24", "12:60", "noon", "-1", "", "1:2:3"] {
            assert!(parse_clock("--time", bad).is_err(), "{bad}");
        }
        let t = start_time(21.0, true);
        assert!(t.paused);
        assert_eq!(t.clock_text(), "21:00");
    }
}
