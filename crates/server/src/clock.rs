//! RFC 3339 UTC timestamps with millisecond precision (`IF-ID-001`),
//! e.g. `2026-10-15T12:34:56.789Z`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SECS_PER_DAY: u64 = 86_400;

pub fn now_rfc3339_ms() -> String {
    format_rfc3339_ms(SystemTime::now())
}

/// Formats `time` as `YYYY-MM-DDTHH:MM:SS.mmmZ`. Times before the Unix epoch
/// are clamped to the epoch; a host clock that far off is a deployment fault.
pub fn format_rfc3339_ms(time: SystemTime) -> String {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let secs = since_epoch.as_secs();
    let millis = since_epoch.subsec_millis();
    let (year, month, day) = civil_from_days(secs / SECS_PER_DAY);
    let secs_of_day = secs % SECS_PER_DAY;
    let (hour, minute, second) = (
        secs_of_day / 3600,
        secs_of_day % 3600 / 60,
        secs_of_day % 60,
    );
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Days since 1970-01-01 to a proleptic Gregorian date
/// (Howard Hinnant, "chrono-Compatible Low-Level Date Algorithms").
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_millis(millis: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(millis)
    }

    #[test]
    fn formats_epoch() {
        assert_eq!(format_rfc3339_ms(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn formats_known_instants() {
        // Reference values from `date -u -r <secs>`.
        assert_eq!(
            format_rfc3339_ms(at_millis(1_700_000_000_123)),
            "2023-11-14T22:13:20.123Z"
        );
        assert_eq!(
            format_rfc3339_ms(at_millis(1_709_164_800_000)),
            "2024-02-29T00:00:00.000Z"
        );
        assert_eq!(
            format_rfc3339_ms(at_millis(951_782_399_999)),
            "2000-02-28T23:59:59.999Z"
        );
    }

    #[test]
    fn has_millisecond_shape() {
        let now = now_rfc3339_ms();
        assert_eq!(now.len(), "2026-10-15T12:34:56.789Z".len(), "{now}");
        assert!(now.ends_with('Z'));
        assert_eq!(&now[19..20], ".");
    }
}
