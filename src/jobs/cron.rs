//! Five-field cron expressions evaluated in local time.
//!
//! Why in-repo: no cron crate is in the dependency tree, the useful ones pull
//! in `chrono`, and flockr already links `time` without its local-offset
//! feature. The grammar is small and the interesting part (daylight-saving
//! transitions) needs the platform's timezone rules either way, which the
//! caller supplies through `UtcOffset`.
//!
//! Grammar: `minute hour day-of-month month day-of-week`. Each field is a
//! comma-separated list of `*`, `a`, `a-b`, `*/n` or `a-b/n`. Month and
//! weekday names (`jan`, `mon`) work as values and range ends; weekday `7` is
//! Sunday. When both day fields are restricted a day matches if either does
//! (Vixie cron semantics).
//!
//! Daylight saving: the schedule follows the wall clock forward.
//! - A time that does not exist (spring forward) fires at the instant the
//!   clock jumps.
//! - A time that happens twice (fall back) fires on its first pass only, and
//!   the repeated hour is not scheduled again, so no run fires twice.

/// Offset of local time from UTC, in seconds, at a Unix timestamp.
pub(crate) trait UtcOffset {
    fn offset_secs(&self, unix_secs: i64) -> i32;
}

impl<F: Fn(i64) -> i32> UtcOffset for F {
    fn offset_secs(&self, unix_secs: i64) -> i32 {
        self(unix_secs)
    }
}

/// The machine's local timezone, from the platform's C library rules.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SystemLocal;

impl UtcOffset for SystemLocal {
    fn offset_secs(&self, unix_secs: i64) -> i32 {
        let Some((year, month, day, hour, minute, second)) =
            crate::platform::local_civil_at(unix_secs)
        else {
            return 0;
        };
        let days = days_from_civil(i64::from(year), u32::from(month), u32::from(day));
        let local =
            days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second);
        i32::try_from(local - unix_secs).unwrap_or(0)
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_index = i64::from((month + 9) % 12);
    let day_of_year = (153 * month_index + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Inverse of `days_from_civil`: (year, month 1-12, day 1-31).
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Weekday of a day count since the epoch: 0 is Sunday.
fn weekday_from_days(days: i64) -> u32 {
    (days + 4).rem_euclid(7) as u32
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CronExpr {
    source: String,
    minutes: u64,
    hours: u32,
    days_of_month: u32,
    months: u16,
    days_of_week: u8,
    dom_star: bool,
    dow_star: bool,
}

struct FieldSpec {
    name: &'static str,
    min: u32,
    max: u32,
    names: &'static [&'static str],
    /// Value of the first entry of `names`.
    names_base: u32,
}

const MINUTE: FieldSpec = FieldSpec {
    name: "minute",
    min: 0,
    max: 59,
    names: &[],
    names_base: 0,
};
const HOUR: FieldSpec = FieldSpec {
    name: "hour",
    min: 0,
    max: 23,
    names: &[],
    names_base: 0,
};
const DAY_OF_MONTH: FieldSpec = FieldSpec {
    name: "day-of-month",
    min: 1,
    max: 31,
    names: &[],
    names_base: 0,
};
const MONTH: FieldSpec = FieldSpec {
    name: "month",
    min: 1,
    max: 12,
    names: &[
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ],
    names_base: 1,
};
const DAY_OF_WEEK: FieldSpec = FieldSpec {
    name: "day-of-week",
    min: 0,
    max: 7,
    names: &["sun", "mon", "tue", "wed", "thu", "fri", "sat"],
    names_base: 0,
};

fn parse_value(text: &str, spec: &FieldSpec) -> Result<u32, String> {
    let lower = text.to_ascii_lowercase();
    if let Some(index) = spec.names.iter().position(|name| *name == lower) {
        return Ok(spec.names_base + index as u32);
    }
    let value: u32 = text.parse().map_err(|_| {
        if spec.names.is_empty() {
            format!("{}: '{text}' is not a number", spec.name)
        } else {
            format!("{}: '{text}' is not a number or name", spec.name)
        }
    })?;
    if value < spec.min || value > spec.max {
        return Err(format!(
            "{}: value {value} is out of range {}-{}",
            spec.name, spec.min, spec.max
        ));
    }
    Ok(value)
}

/// Parse one field into a bit set (bit n set when value n matches) and
/// whether the field starts with `*`.
fn parse_field(text: &str, spec: &FieldSpec) -> Result<(u64, bool), String> {
    if text.is_empty() {
        return Err(format!("{}: field is empty", spec.name));
    }
    let star = text.starts_with('*');
    let mut bits = 0u64;
    for item in text.split(',') {
        if item.is_empty() {
            return Err(format!("{}: empty list item in '{text}'", spec.name));
        }
        let (range, step) = match item.split_once('/') {
            Some((range, step)) => {
                let step: u32 = step
                    .parse()
                    .map_err(|_| format!("{}: step '{step}' is not a number", spec.name))?;
                if step == 0 {
                    return Err(format!("{}: step must be at least 1", spec.name));
                }
                (range, Some(step))
            }
            None => (item, None),
        };
        let (low, high) = if range == "*" {
            (spec.min, if spec.max == 7 { 6 } else { spec.max })
        } else if let Some((low, high)) = range.split_once('-') {
            let low = parse_value(low, spec)?;
            let high = parse_value(high, spec)?;
            if low > high {
                return Err(format!("{}: range '{range}' runs backwards", spec.name));
            }
            (low, high)
        } else {
            if step.is_some() {
                return Err(format!(
                    "{}: '{item}' needs a range or '*' before the step, such as '*/{}'",
                    spec.name,
                    step.unwrap_or(1)
                ));
            }
            let value = parse_value(range, spec)?;
            (value, value)
        };
        let step = step.unwrap_or(1);
        let mut value = low;
        while value <= high {
            // Weekday 7 is Sunday.
            let bit = if spec.max == 7 && value == 7 {
                0
            } else {
                value
            };
            bits |= 1u64 << bit;
            value += step;
        }
    }
    Ok((bits, star))
}

impl CronExpr {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let fields: Vec<&str> = text.split_whitespace().collect();
        if fields.len() != 5 {
            let hint = if text.trim_start().starts_with('@') {
                " (@-macros such as @daily are not supported; write the five fields)"
            } else {
                ""
            };
            return Err(format!(
                "expected 5 fields (minute hour day-of-month month day-of-week), found {}{hint}",
                fields.len()
            ));
        }
        let (minutes, _) = parse_field(fields[0], &MINUTE)?;
        let (hours, _) = parse_field(fields[1], &HOUR)?;
        let (days_of_month, dom_star) = parse_field(fields[2], &DAY_OF_MONTH)?;
        let (months, _) = parse_field(fields[3], &MONTH)?;
        let (days_of_week, dow_star) = parse_field(fields[4], &DAY_OF_WEEK)?;
        Ok(Self {
            source: fields.join(" "),
            minutes,
            hours: hours as u32,
            days_of_month: days_of_month as u32,
            months: months as u16,
            days_of_week: days_of_week as u8,
            dom_star,
            dow_star,
        })
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.source
    }

    fn day_matches(&self, day: u32, days: i64) -> bool {
        let dom = self.days_of_month & (1 << day) != 0;
        let dow = self.days_of_week & (1 << weekday_from_days(days)) != 0;
        if self.dom_star || self.dow_star {
            dom && dow
        } else {
            dom || dow
        }
    }

    /// The first scheduled instant strictly after `after_unix_secs`, in Unix
    /// seconds, or `None` when the expression never fires (for example
    /// `0 0 31 2 *`).
    pub(crate) fn next_after(&self, after_unix_secs: i64, zone: &dyn UtcOffset) -> Option<i64> {
        let start_wall = after_unix_secs + i64::from(zone.offset_secs(after_unix_secs));
        let mut wall = (start_wall.div_euclid(60) + 1) * 60;
        // Eight years covers the longest gap between leap days.
        let limit = wall + 8 * 366 * 86_400;
        while wall <= limit {
            let days = wall.div_euclid(86_400);
            let second_of_day = wall.rem_euclid(86_400);
            let (year, month, day) = civil_from_days(days);
            if self.months & (1 << month) == 0 {
                wall = days_from_civil(
                    year + i64::from(month == 12),
                    if month == 12 { 1 } else { month + 1 },
                    1,
                ) * 86_400;
                continue;
            }
            if !self.day_matches(day, days) {
                wall = (days + 1) * 86_400;
                continue;
            }
            let hour = (second_of_day / 3_600) as u32;
            if self.hours & (1 << hour) == 0 {
                wall = days * 86_400 + i64::from(hour + 1) * 3_600;
                continue;
            }
            let minute = (second_of_day % 3_600 / 60) as u32;
            if self.minutes & (1 << minute) == 0 {
                wall += 60;
                continue;
            }
            if let Some(instant) = resolve_wall(wall, after_unix_secs, zone) {
                return Some(instant);
            }
            wall += 60;
        }
        None
    }
}

/// Turn a local wall-clock second count into a Unix timestamp strictly after
/// `after`. A wall time inside a spring-forward gap resolves to the instant
/// the clock jumps; one that repeats resolves to its earliest pass after
/// `after`.
fn resolve_wall(wall: i64, after: i64, zone: &dyn UtcOffset) -> Option<i64> {
    let before = i64::from(zone.offset_secs(wall - 86_400));
    let later = i64::from(zone.offset_secs(wall + 86_400));
    let mut candidates = [wall - before, wall - later];
    candidates.sort_unstable();
    let valid = |instant: i64| instant + i64::from(zone.offset_secs(instant)) == wall;
    let mut matches = Vec::with_capacity(2);
    for candidate in candidates {
        if valid(candidate) && !matches.contains(&candidate) {
            matches.push(candidate);
        }
    }
    if matches.is_empty() {
        // Gap: the first instant whose wall clock reads at or after `wall`.
        let (mut low, mut high) = (candidates[0], candidates[1]);
        while low < high {
            let middle = low + (high - low) / 2;
            if middle + i64::from(zone.offset_secs(middle)) >= wall {
                high = middle;
            } else {
                low = middle + 1;
            }
        }
        return (high > after).then_some(high);
    }
    matches.into_iter().find(|instant| *instant > after)
}

#[cfg(test)]
mod tests {
    use super::*;

    const UTC: fn(i64) -> i32 = |_| 0;

    fn unix(year: i64, month: u32, day: u32, hour: i64, minute: i64) -> i64 {
        days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60
    }

    /// A US-eastern-like zone: EST (-5h), EDT (-4h) from 2026-03-08 07:00Z to
    /// 2026-11-01 06:00Z.
    fn eastern(unix_secs: i64) -> i32 {
        let start = unix(2026, 3, 8, 7, 0);
        let end = unix(2026, 11, 1, 6, 0);
        if unix_secs >= start && unix_secs < end {
            -4 * 3_600
        } else {
            -5 * 3_600
        }
    }

    fn next(expr: &str, after: i64, zone: &dyn UtcOffset) -> Option<i64> {
        CronExpr::parse(expr).unwrap().next_after(after, zone)
    }

    #[test]
    fn civil_conversions_round_trip() {
        for days in [
            -1_000_000, -1, 0, 1, 59, 60, 11_016, 19_000, 25_000, 1_000_000,
        ] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(weekday_from_days(0), 4, "1970-01-01 was a Thursday");
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    }

    #[test]
    fn parses_lists_ranges_steps_and_names() {
        let expr = CronExpr::parse("0,30 9-17/2 * jan-mar mon-fri").unwrap();
        assert_eq!(expr.minutes, (1 << 0) | (1 << 30));
        assert_eq!(
            expr.hours,
            (1 << 9) | (1 << 11) | (1 << 13) | (1 << 15) | (1 << 17)
        );
        assert_eq!(expr.months, (1 << 1) | (1 << 2) | (1 << 3));
        assert_eq!(expr.days_of_week, 0b0111110);
        assert!(expr.dom_star);
        assert!(!expr.dow_star);
        assert_eq!(expr.as_str(), "0,30 9-17/2 * jan-mar mon-fri");
    }

    #[test]
    fn weekday_seven_is_sunday() {
        let seven = CronExpr::parse("0 0 * * 7").unwrap();
        let zero = CronExpr::parse("0 0 * * 0").unwrap();
        assert_eq!(seven.days_of_week, zero.days_of_week);
        let range = CronExpr::parse("0 0 * * 5-7").unwrap();
        assert_eq!(range.days_of_week, (1 << 5) | (1 << 6) | 1);
        assert_eq!(CronExpr::parse("0 0 * * *").unwrap().days_of_week, 0x7f);
    }

    #[test]
    fn rejects_malformed_expressions_with_a_reason() {
        for (text, needle) in [
            ("0 10 * *", "expected 5 fields"),
            ("0 10 * * * *", "expected 5 fields"),
            ("@daily", "@-macros"),
            ("60 * * * *", "minute: value 60 is out of range 0-59"),
            ("* 24 * * *", "hour: value 24 is out of range 0-23"),
            ("* * 0 * *", "day-of-month: value 0 is out of range 1-31"),
            ("* * * 13 *", "month: value 13 is out of range 1-12"),
            ("* * * * 8", "day-of-week: value 8 is out of range 0-7"),
            ("*/0 * * * *", "step must be at least 1"),
            ("5/10 * * * *", "needs a range or '*'"),
            ("10-5 * * * *", "runs backwards"),
            ("a * * * *", "minute: 'a' is not a number"),
            ("* * * foo *", "month: 'foo' is not a number or name"),
            ("1,,2 * * * *", "empty list item"),
            ("*/x * * * *", "step 'x' is not a number"),
        ] {
            let error = CronExpr::parse(text).unwrap_err();
            assert!(
                error.contains(needle),
                "{text:?} gave {error:?}, expected it to contain {needle:?}"
            );
        }
    }

    #[test]
    fn next_time_in_utc() {
        let after = unix(2026, 10, 18, 9, 59);
        assert_eq!(
            next("0 10 * * *", after, &UTC),
            Some(unix(2026, 10, 18, 10, 0))
        );
        // Strictly after: the matching minute itself does not count.
        assert_eq!(
            next("0 10 * * *", unix(2026, 10, 18, 10, 0), &UTC),
            Some(unix(2026, 10, 19, 10, 0))
        );
        assert_eq!(
            next("*/15 * * * *", unix(2026, 10, 18, 10, 7) + 30, &UTC),
            Some(unix(2026, 10, 18, 10, 15))
        );
        // 2026-10-18 is a Sunday; weekdays 1-5 means Monday the 19th.
        assert_eq!(
            next("0 10 * * 1-5", after, &UTC),
            Some(unix(2026, 10, 19, 10, 0))
        );
    }

    #[test]
    fn day_of_month_or_weekday_when_both_are_restricted() {
        // The 20th or any Monday. 2026-10-18 is Sunday: Monday the 19th wins.
        assert_eq!(
            next("0 0 20 * 1", unix(2026, 10, 18, 12, 0), &UTC),
            Some(unix(2026, 10, 19, 0, 0))
        );
        // After the 19th, the 20th (a Tuesday) matches via day-of-month.
        assert_eq!(
            next("0 0 20 * 1", unix(2026, 10, 19, 12, 0), &UTC),
            Some(unix(2026, 10, 20, 0, 0))
        );
        // A star in either day field keeps it a plain AND.
        assert_eq!(
            next("0 0 20 * *", unix(2026, 10, 18, 12, 0), &UTC),
            Some(unix(2026, 10, 20, 0, 0))
        );
    }

    #[test]
    fn leap_days_and_impossible_dates() {
        assert_eq!(
            next("0 0 29 2 *", unix(2026, 1, 1, 0, 0), &UTC),
            Some(unix(2028, 2, 29, 0, 0))
        );
        assert_eq!(next("0 0 31 2 *", unix(2026, 1, 1, 0, 0), &UTC), None);
        assert_eq!(
            next("0 0 31 * *", unix(2026, 4, 1, 0, 0), &UTC),
            Some(unix(2026, 5, 31, 0, 0))
        );
    }

    #[test]
    fn year_rollover() {
        assert_eq!(
            next("30 23 31 12 *", unix(2026, 12, 31, 23, 30), &UTC),
            Some(unix(2027, 12, 31, 23, 30))
        );
        assert_eq!(
            next("0 0 1 1 *", unix(2026, 12, 31, 23, 59), &UTC),
            Some(unix(2027, 1, 1, 0, 0))
        );
    }

    #[test]
    fn local_time_uses_the_zone_offset() {
        // 10:00 EST is 15:00Z in winter and 10:00 EDT is 14:00Z in summer.
        assert_eq!(
            next("0 10 * * *", unix(2026, 1, 15, 0, 0), &eastern),
            Some(unix(2026, 1, 15, 15, 0))
        );
        assert_eq!(
            next("0 10 * * *", unix(2026, 7, 15, 0, 0), &eastern),
            Some(unix(2026, 7, 15, 14, 0))
        );
    }

    #[test]
    fn spring_forward_gap_fires_when_the_clock_jumps() {
        // 02:30 local does not exist on 2026-03-08; the job fires at 03:00 EDT
        // (07:00Z), the instant the clock jumps.
        let after = unix(2026, 3, 8, 5, 0);
        let fired = next("30 2 * * *", after, &eastern).unwrap();
        assert_eq!(fired, unix(2026, 3, 8, 7, 0));
        // And it does not fire a second time that day.
        assert_eq!(
            next("30 2 * * *", fired, &eastern),
            Some(unix(2026, 3, 9, 6, 30))
        );
    }

    #[test]
    fn spring_forward_does_not_disturb_other_times() {
        assert_eq!(
            next("0 4 * * *", unix(2026, 3, 8, 5, 0), &eastern),
            Some(unix(2026, 3, 8, 8, 0))
        );
        // Hourly jobs: 02:15 is in the gap and fires at 03:00; 03:15 is normal.
        let first = next("15 * * * *", unix(2026, 3, 8, 6, 20), &eastern).unwrap();
        assert_eq!(first, unix(2026, 3, 8, 7, 0));
        let second = next("15 * * * *", first, &eastern).unwrap();
        assert_eq!(second, unix(2026, 3, 8, 7, 15));
    }

    #[test]
    fn fall_back_fires_once_on_the_first_pass() {
        // 01:30 happens at 05:30Z (EDT) and again at 06:30Z (EST).
        let first = next("30 1 * * *", unix(2026, 11, 1, 0, 0), &eastern).unwrap();
        assert_eq!(first, unix(2026, 11, 1, 5, 30));
        let after_first = next("30 1 * * *", first, &eastern).unwrap();
        assert_eq!(
            after_first,
            unix(2026, 11, 2, 6, 30),
            "next day, not the repeat"
        );
    }

    #[test]
    fn frequent_jobs_never_fire_twice_across_fall_back() {
        let mut after = unix(2026, 11, 1, 3, 50);
        let mut seen = Vec::new();
        for _ in 0..12 {
            let fired = next("*/30 * * * *", after, &eastern).unwrap();
            assert!(fired > after, "schedule must move forward");
            seen.push(fired);
            after = fired;
        }
        let mut sorted = seen.clone();
        sorted.dedup();
        assert_eq!(sorted, seen, "no duplicate instants");
        // The wall clock never jumps backward, so it keeps advancing past the
        // repeated hour.
        assert!(seen.last().unwrap() > &unix(2026, 11, 1, 8, 0));
    }

    #[cfg(unix)]
    #[test]
    fn system_local_matches_a_posix_timezone_rule() {
        // POSIX TZ strings need no timezone database, so this runs anywhere.
        // The test runner isolates each test in its own process.
        std::env::set_var("TZ", "EST5EDT,M3.2.0/2,M11.1.0/2");
        extern "C" {
            fn tzset();
        }
        // SAFETY: tzset takes no arguments and only rereads TZ.
        unsafe { tzset() };
        let zone = SystemLocal;
        assert_eq!(zone.offset_secs(unix(2026, 1, 15, 12, 0)), -5 * 3_600);
        assert_eq!(zone.offset_secs(unix(2026, 7, 15, 12, 0)), -4 * 3_600);
        let fired = CronExpr::parse("30 2 * * *")
            .unwrap()
            .next_after(unix(2026, 3, 8, 5, 0), &zone)
            .unwrap();
        assert_eq!(fired, unix(2026, 3, 8, 7, 0));
        let fired = CronExpr::parse("0 10 * * *")
            .unwrap()
            .next_after(unix(2026, 7, 15, 0, 0), &zone)
            .unwrap();
        assert_eq!(fired, unix(2026, 7, 15, 14, 0));
    }
}
