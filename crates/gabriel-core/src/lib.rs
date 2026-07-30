//! Shared types for Gabriel: the on-disk request format, template resolution,
//! captured traffic, and response comparison.
//!
//! Nothing in this crate performs I/O beyond reading the clock and the process
//! environment, which keeps the format definition testable in isolation from
//! the engine that executes it.

pub mod capture;
pub mod diff;
pub mod error;
pub mod jsonpath;
pub mod model;
pub mod response;
pub mod vars;

pub use error::{Error, Result};
pub use model::RequestSpec;
pub use response::ExecutedResponse;

use base64::Engine as _;

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn b64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn b64_decode(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(text).ok()
}

/// Serde helper: bytes on the wire as base64, so a stored response stays valid
/// JSON even when the body isn't UTF-8.
pub(crate) mod b64_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::b64_encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        super::b64_decode(&text).ok_or_else(|| serde::de::Error::custom("invalid base64"))
    }
}

/// Format epoch milliseconds as `2026-07-29T13:54:00.123Z`.
///
/// Hand-rolled rather than pulled in with a date library: Gabriel needs exactly
/// one direction of one format, and a dependency that ships its own tzdb is a
/// poor trade for that.
pub fn format_iso8601(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let millis = ms % 1000;
    let days = secs.div_euclid(86_400);
    let time_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (time_of_day / 3600, (time_of_day % 3600) / 60, time_of_day % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Calendar date `offset_days` away from an instant, as `(year, month, day)`.
/// Used for certificate validity windows, where only whole days matter.
pub fn date_parts(ms: u64, offset_days: i64) -> (i64, u32, u32) {
    let days = (ms / 1000) as i64 / 86_400 + offset_days;
    civil_from_days(days)
}

/// Howard Hinnant's `civil_from_days`: days since the epoch to a calendar date.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Human-friendly byte count for terminal output.
pub fn format_bytes(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_a_known_instant() {
        assert_eq!(format_iso8601(1_785_283_200_000), "2026-07-29T00:00:00.000Z");
    }

    #[test]
    fn formats_the_epoch_and_a_leap_day() {
        assert_eq!(format_iso8601(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_iso8601(1_709_210_096_789), "2024-02-29T12:34:56.789Z");
    }

    #[test]
    fn base64_round_trips() {
        let bytes = vec![0u8, 1, 254, 255];
        assert_eq!(b64_decode(&b64_encode(&bytes)).unwrap(), bytes);
    }

    #[test]
    fn byte_counts_stay_readable() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
    }
}
