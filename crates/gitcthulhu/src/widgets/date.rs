//! Date formatting (spec 21 §1.1, Gitnuro `DateExtensions.kt`, 08 §0.5): relative "Today"/"Yesterday", the
//! MEDIUM English format (`Nov 11, 2026`), Java-style custom patterns and 24-hour times.
//!
//! The functions are pure: the current time and the local UTC offset are passed in.

use crate::backend::GitTime;
use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, Timelike};

use crate::settings::Settings;

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// The Date/Time settings that affect formatting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DateFormat {
    /// Use the system (MEDIUM English) format instead of `custom_pattern`.
    pub use_system: bool,
    /// Java-style pattern such as `dd MMM yyyy`.
    pub custom_pattern: String,
    /// 24-hour clock for custom formats.
    pub h24: bool,
    /// Replace today and yesterday with words.
    pub relative: bool,
}

impl DateFormat {
    /// The formatting options of the current settings.
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            use_system: settings.date_use_system,
            custom_pattern: settings.date_custom_format.clone(),
            h24: settings.date_24h,
            relative: settings.date_relative,
        }
    }
}

impl Default for DateFormat {
    fn default() -> Self {
        Self::from_settings(&Settings::default())
    }
}

/// The local UTC offset in minutes at `epoch_seconds` for the current machine.
pub fn local_offset_minutes(epoch_seconds: i64) -> i32 {
    use chrono::{Local, Offset, TimeZone};
    Local
        .timestamp_opt(epoch_seconds, 0)
        .single()
        .map(|t| t.offset().fix().local_minus_utc() / 60)
        .unwrap_or(0)
}

fn local(time_seconds: i64, offset_minutes: i32) -> Option<DateTime<FixedOffset>> {
    let offset = FixedOffset::east_opt(offset_minutes.checked_mul(60)?)?;
    DateTime::from_timestamp(time_seconds, 0).map(|t| t.with_timezone(&offset))
}

/// Formats `time` for display in the zone `offset_minutes`, relative to `now` (same zone).
///
/// With `show_time` a time follows the date after a space: English SHORT (`3:45 PM`) in the system format,
/// `HH:mm` or `hh:mm a` in custom formats.
pub fn format_date(
    time: GitTime,
    now: GitTime,
    offset_minutes: i32,
    format: &DateFormat,
    show_time: bool,
) -> String {
    let Some(at) = local(time.seconds, offset_minutes) else {
        return String::new();
    };
    let today = local(now.seconds, offset_minutes).map(|t| t.date_naive());
    let mut out = if format.relative && today == Some(at.date_naive()) {
        "Today".to_owned()
    } else if format.relative && today.and_then(|d| d.pred_opt()) == Some(at.date_naive()) {
        "Yesterday".to_owned()
    } else if format.use_system {
        format!(
            "{} {}, {}",
            &MONTHS[at.month0() as usize][..3],
            at.day(),
            at.year()
        )
    } else {
        format_pattern(&format.custom_pattern, &at)
    };
    if show_time {
        out.push(' ');
        if format.use_system {
            out.push_str(&format_pattern("h:mm a", &at));
        } else if format.h24 {
            out.push_str(&format_pattern("HH:mm", &at));
        } else {
            out.push_str(&format_pattern("hh:mm a", &at));
        }
    }
    out
}

/// Formats `at` with a Java `DateTimeFormatter` style pattern. Supported letters: `y M d E H h m s a`; text in
/// single quotes (`''` is a quote) and every other character are copied.
pub fn format_pattern(pattern: &str, at: &DateTime<FixedOffset>) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            if chars.get(i + 1) == Some(&'\'') {
                out.push('\'');
                i += 2;
                continue;
            }
            i += 1;
            while i < chars.len() && chars[i] != '\'' {
                out.push(chars[i]);
                i += 1;
            }
            i += 1;
            continue;
        }
        if !matches!(c, 'y' | 'M' | 'd' | 'E' | 'H' | 'h' | 'm' | 's' | 'a') {
            out.push(c);
            i += 1;
            continue;
        }
        let mut count = 1;
        while chars.get(i + count) == Some(&c) {
            count += 1;
        }
        i += count;
        push_field(&mut out, c, count, at);
    }
    out
}

fn push_field(out: &mut String, letter: char, count: usize, at: &DateTime<FixedOffset>) {
    let number = |out: &mut String, value: u32| out.push_str(&format!("{value:0count$}"));
    match letter {
        'y' if count == 2 => out.push_str(&format!("{:02}", at.year().rem_euclid(100))),
        'y' => out.push_str(&format!("{:0count$}", at.year())),
        'M' => {
            let name = MONTHS[at.month0() as usize];
            match count {
                1 | 2 => number(out, at.month()),
                3 => out.push_str(&name[..3]),
                _ => out.push_str(name),
            }
        }
        'd' => number(out, at.day()),
        'E' => {
            let name = DAYS[at.weekday().num_days_from_monday() as usize];
            if count >= 4 {
                out.push_str(name);
            } else {
                out.push_str(&name[..3]);
            }
        }
        'H' => number(out, at.hour()),
        'h' => number(
            out,
            if at.hour().is_multiple_of(12) {
                12
            } else {
                at.hour() % 12
            },
        ),
        'm' => number(out, at.minute()),
        's' => number(out, at.second()),
        'a' => out.push_str(if at.hour() < 12 { "AM" } else { "PM" }),
        _ => {}
    }
}

/// The local day containing `time` as a day number (days since 1 January of year 1), used as a cache key for
/// per-day formatting.
pub fn day_key(time: GitTime, offset_minutes: i32) -> i64 {
    local(time.seconds, offset_minutes)
        .map(|t| {
            let date: NaiveDate = t.date_naive();
            date.num_days_from_ce() as i64
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> GitTime {
        let secs = NaiveDate::from_ymd_opt(y, mo, d)
            .and_then(|d| d.and_hms_opt(h, mi, 0))
            .expect("valid date")
            .and_utc()
            .timestamp();
        GitTime {
            seconds: secs,
            offset_minutes: 0,
        }
    }

    fn fmt(time: GitTime, now: GitTime, format: &DateFormat, show_time: bool) -> String {
        format_date(time, now, 0, format, show_time)
    }

    #[test]
    fn today_and_yesterday_are_words_when_relative() {
        let now = at(2026, 11, 11, 15, 30);
        let f = DateFormat::default();
        assert_eq!(fmt(at(2026, 11, 11, 1, 0), now, &f, false), "Today");
        assert_eq!(fmt(at(2026, 11, 10, 23, 59), now, &f, false), "Yesterday");
        assert_eq!(
            fmt(at(2026, 11, 9, 12, 0), now, &f, false),
            "Nov 9, 2026",
            "older dates use the MEDIUM English format"
        );
    }

    #[test]
    fn relative_off_always_prints_the_date() {
        let now = at(2026, 11, 11, 15, 30);
        let f = DateFormat {
            relative: false,
            ..DateFormat::default()
        };
        assert_eq!(fmt(at(2026, 11, 11, 1, 0), now, &f, false), "Nov 11, 2026");
    }

    #[test]
    fn custom_pattern_and_time() {
        let now = at(2026, 1, 1, 0, 0);
        let t = at(2025, 3, 7, 16, 5);
        let f = DateFormat {
            use_system: false,
            custom_pattern: "dd MMM yyyy".into(),
            h24: true,
            relative: true,
        };
        assert_eq!(fmt(t, now, &f, false), "07 Mar 2025");
        assert_eq!(fmt(t, now, &f, true), "07 Mar 2025 16:05", "24 hour clock");
        let twelve = DateFormat { h24: false, ..f };
        assert_eq!(
            fmt(t, now, &twelve, true),
            "07 Mar 2025 04:05 PM",
            "12 hour clock"
        );
    }

    #[test]
    fn system_time_is_english_short() {
        let now = at(2026, 1, 1, 0, 0);
        assert_eq!(
            fmt(at(2025, 3, 7, 0, 5), now, &DateFormat::default(), true),
            "Mar 7, 2025 12:05 AM"
        );
    }

    #[test]
    fn patterns_support_quotes_names_and_weekdays() {
        let now = at(2026, 1, 1, 0, 0);
        let f = DateFormat {
            use_system: false,
            custom_pattern: "EEEE, d MMMM yy 'at' H".into(),
            h24: true,
            relative: false,
        };
        assert_eq!(
            fmt(at(2025, 3, 7, 9, 0), now, &f, false),
            "Friday, 7 March 25 at 9"
        );
    }

    #[test]
    fn offset_moves_the_day_boundary() {
        let now = at(2026, 11, 11, 10, 0);
        let f = DateFormat::default();
        // 23:30 UTC on the 10th is already the 11th at UTC+2.
        let t = at(2026, 11, 10, 23, 30);
        assert_eq!(format_date(t, now, 120, &f, false), "Today");
        assert_eq!(format_date(t, now, 0, &f, false), "Yesterday");
    }
}
