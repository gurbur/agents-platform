//! RFC 3339 UTC timestamps with millisecond precision (`IF-ID-001`),
//! e.g. `2026-10-15T12:34:56.789Z`, and the injectable [`Clock`] presence
//! reads its time from.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SECS_PER_DAY: u64 = 86_400;

pub fn now_rfc3339_ms() -> String {
    format_rfc3339_ms(SystemTime::now())
}

/// The time source of presence (TTLs, the `last_seen_at` throttle and its
/// timestamps). The production clock is the system clock; an
/// [`Clock::adjustable`] one runs with it shifted by an offset that only
/// its [`ClockControl`] moves forward, so a test can pass a TTL or a
/// throttle interval without waiting.
#[derive(Debug, Clone, Default)]
pub struct Clock {
    /// Milliseconds added to the system clock; `None` for the system clock.
    offset_ms: Option<Arc<AtomicU64>>,
}

/// Moves an adjustable [`Clock`] (and every clone of it) forward.
#[derive(Debug, Clone)]
pub struct ClockControl {
    offset_ms: Arc<AtomicU64>,
}

impl Clock {
    pub fn system() -> Self {
        Self::default()
    }

    /// A clock that starts at the system time and can be moved forward.
    pub fn adjustable() -> (Self, ClockControl) {
        let offset_ms = Arc::new(AtomicU64::new(0));
        let clock = Self {
            offset_ms: Some(Arc::clone(&offset_ms)),
        };
        (clock, ClockControl { offset_ms })
    }

    fn offset(&self) -> Duration {
        self.offset_ms.as_ref().map_or(Duration::ZERO, |offset| {
            Duration::from_millis(offset.load(Ordering::Relaxed))
        })
    }

    /// Wall-clock time, truncated to whole milliseconds so that a stored
    /// time and its RFC 3339 text are the same instant.
    pub fn now(&self) -> SystemTime {
        truncate_to_millis(SystemTime::now() + self.offset())
    }

    /// Monotonic time for intervals.
    pub fn instant(&self) -> Instant {
        Instant::now() + self.offset()
    }
}

impl ClockControl {
    pub fn advance(&self, by: Duration) {
        let millis = u64::try_from(by.as_millis()).unwrap_or(u64::MAX);
        self.offset_ms.fetch_add(millis, Ordering::Relaxed);
    }
}

fn truncate_to_millis(time: SystemTime) -> SystemTime {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let millis = u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX);
    UNIX_EPOCH + Duration::from_millis(millis)
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

/// Parses exactly the [`format_rfc3339_ms`] shape back into a time. Anything
/// else (another offset, missing millis, out-of-range fields, a time before
/// the epoch) is `None`.
pub fn parse_rfc3339_ms(text: &str) -> Option<SystemTime> {
    let bytes = text.as_bytes();
    if bytes.len() != RFC3339_MS_LEN {
        return None;
    }
    let separators = [
        (4, b'-'),
        (7, b'-'),
        (10, b'T'),
        (13, b':'),
        (16, b':'),
        (19, b'.'),
        (23, b'Z'),
    ];
    if separators.iter().any(|&(index, byte)| bytes[index] != byte) {
        return None;
    }
    let field = |range: std::ops::Range<usize>| -> Option<u64> {
        let digits = &text[range];
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok()
    };
    let (year, month, day) = (field(0..4)?, field(5..7)?, field(8..10)?);
    let (hour, minute, second, millis) = (
        field(11..13)?,
        field(14..16)?,
        field(17..19)?,
        field(20..23)?,
    );
    if year < 1970 || !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let secs = days * SECS_PER_DAY + hour * 3600 + minute * 60 + second;
    Some(UNIX_EPOCH + Duration::from_secs(secs) + Duration::from_millis(millis))
}

/// Length of `YYYY-MM-DDTHH:MM:SS.mmmZ`.
const RFC3339_MS_LEN: usize = 24;

fn days_in_month(year: u64, month: u64) -> u64 {
    match month {
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Inverse of [`civil_from_days`] for dates on or after 1970-01-01.
fn days_from_civil(year: u64, month: u64, day: u64) -> u64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year / 400;
    let year_of_era = year - era * 400;
    let month_index = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
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
    fn parse_inverts_format() {
        for millis in [
            0,
            951_782_399_999,
            1_700_000_000_123,
            1_709_164_800_000,
            4_102_444_800_001,
        ] {
            let time = at_millis(millis);
            assert_eq!(
                parse_rfc3339_ms(&format_rfc3339_ms(time)),
                Some(time),
                "{millis}"
            );
        }
        let now = SystemTime::now();
        let text = format_rfc3339_ms(now);
        let parsed = parse_rfc3339_ms(&text).unwrap();
        assert!(parsed <= now && now.duration_since(parsed).unwrap() < Duration::from_millis(1));
    }

    #[test]
    fn parse_rejects_other_shapes() {
        for text in [
            "",
            "2026-10-15T12:34:56Z",
            "2026-10-15T12:34:56.789+00:00",
            "2026-10-15 12:34:56.789Z",
            "2026-13-01T00:00:00.000Z",
            "2026-02-29T00:00:00.000Z",
            "2026-10-15T24:00:00.000Z",
            "1969-12-31T23:59:59.999Z",
            "2026-10-15T12:34:5x.789Z",
            "+026-10-15T12:34:56.789Z",
        ] {
            assert_eq!(parse_rfc3339_ms(text), None, "{text:?}");
        }
        assert!(parse_rfc3339_ms("2024-02-29T00:00:00.000Z").is_some());
    }

    #[test]
    fn an_adjustable_clock_moves_forward_with_its_control_only() {
        let system = Clock::system();
        let (clock, control) = Clock::adjustable();
        let copy = clock.clone();
        let before = clock.now();
        let instant = clock.instant();
        control.advance(Duration::from_secs(3600));
        assert!(copy.now().duration_since(before).unwrap() >= Duration::from_secs(3600));
        assert!(copy.instant() - instant >= Duration::from_secs(3600));
        assert!(clock.now().duration_since(system.now()).unwrap() >= Duration::from_secs(3599));
    }

    #[test]
    fn clock_time_is_whole_milliseconds() {
        let now = Clock::system().now();
        let since_epoch = now.duration_since(UNIX_EPOCH).unwrap();
        assert_eq!(since_epoch.subsec_nanos() % 1_000_000, 0);
        assert_eq!(parse_rfc3339_ms(&format_rfc3339_ms(now)), Some(now));
    }

    #[test]
    fn has_millisecond_shape() {
        let now = now_rfc3339_ms();
        assert_eq!(now.len(), "2026-10-15T12:34:56.789Z".len(), "{now}");
        assert!(now.ends_with('Z'));
        assert_eq!(&now[19..20], ".");
    }
}
