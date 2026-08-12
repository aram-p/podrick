//! Natural-language date parsing, with an injectable clock so every test is
//! deterministic. No recurrence — that is deliberately out of scope.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveTime, TimeZone, Weekday};

#[derive(Debug, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A parsed due value. `None` means the caller asked to clear it.
pub type Parsed = Option<String>;

/// Parse a due expression against `now`.
///
/// Two rules that people get wrong, both pinned by tests below:
///   * a weekday name means **today** if today is that weekday;
///   * a bare time that has already passed means **tomorrow**.
pub fn parse(input: &str, now: DateTime<Local>) -> Result<Parsed, ParseError> {
    let raw = input.trim().to_lowercase();
    if raw.is_empty() || raw == "none" || raw == "clear" {
        return Ok(None);
    }

    // Whole-string ISO forms first — they are unambiguous and worth short-circuiting.
    if let Some(v) = parse_iso(&raw) {
        return Ok(Some(v));
    }

    let tokens: Vec<&str> = raw.split_whitespace().collect();

    // Peel a trailing time off the end: "friday 3pm", "friday at 3 pm".
    let (date_tokens, time) = split_time(&tokens);

    // "in 3 hours" is relative to the instant, not the day, so it is handled whole.
    if let Some(dt) = parse_relative_instant(&date_tokens, now) {
        return Ok(Some(fmt_datetime(dt.date_naive(), dt.time())));
    }

    let date = parse_date(&date_tokens, now)?;

    match (date, time) {
        (Some(d), Some(t)) => Ok(Some(fmt_datetime(d, t))),
        (Some(d), None) => Ok(Some(fmt_date(d))),
        (None, Some(t)) => {
            // Bare time: today if still ahead of us, otherwise tomorrow.
            let today = now.date_naive();
            let d = if t > now.time() {
                today
            } else {
                today + Duration::days(1)
            };
            Ok(Some(fmt_datetime(d, t)))
        }
        (None, None) => Err(ParseError(format!(
            "could not understand the date {input:?}"
        ))),
    }
}

fn fmt_date(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn fmt_datetime(d: NaiveDate, t: NaiveTime) -> String {
    format!("{}T{}", d.format("%Y-%m-%d"), t.format("%H:%M"))
}

/// Does a stored due value carry a time?
pub fn has_time(due: &str) -> bool {
    due.contains('T')
}

pub fn date_part(due: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(due.split('T').next()?, "%Y-%m-%d").ok()
}

fn parse_iso(s: &str) -> Option<String> {
    // 2026-12-25T15:00 / 2026-12-25 15:00 / 2026-12-25.
    // Input arrives lowercased, so the ISO separator may be a lowercase `t`.
    let normalized = s.replacen(' ', "T", 1).replacen('t', "T", 1);
    if let Some((d, t)) = normalized.split_once('T') {
        let date = NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()?;
        let time = NaiveTime::parse_from_str(t, "%H:%M")
            .or_else(|_| NaiveTime::parse_from_str(t, "%H:%M:%S"))
            .ok()?;
        return Some(fmt_datetime(date, time));
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok().map(fmt_date)
}

/// Split trailing time tokens off a token list. Handles "3pm", "3 pm", "at 3pm", "15:00".
fn split_time<'a>(tokens: &[&'a str]) -> (Vec<&'a str>, Option<NaiveTime>) {
    let mut toks = tokens.to_vec();

    // "3 pm" — join a trailing bare meridiem onto the number before it.
    if toks.len() >= 2 {
        let last = toks[toks.len() - 1];
        if last == "am" || last == "pm" {
            let joined = format!("{}{}", toks[toks.len() - 2], last);
            if let Some(t) = parse_time(&joined) {
                toks.truncate(toks.len() - 2);
                strip_at(&mut toks);
                return (toks, Some(t));
            }
        }
    }

    if let Some(&last) = toks.last() {
        if let Some(t) = parse_time(last) {
            toks.pop();
            strip_at(&mut toks);
            return (toks, Some(t));
        }
    }

    (toks, None)
}

fn strip_at(toks: &mut Vec<&str>) {
    if toks.last() == Some(&"at") {
        toks.pop();
    }
}

fn parse_time(s: &str) -> Option<NaiveTime> {
    match s {
        "noon" => return NaiveTime::from_hms_opt(12, 0, 0),
        "midnight" => return NaiveTime::from_hms_opt(0, 0, 0),
        _ => {}
    }

    // 3pm / 3:30pm / 11am
    for suffix in ["am", "pm"] {
        if let Some(head) = s.strip_suffix(suffix) {
            let (h, m) = match head.split_once(':') {
                Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
                None => (head.parse::<u32>().ok()?, 0),
            };
            if h == 0 || h > 12 || m > 59 {
                return None;
            }
            let h24 = match (suffix, h) {
                ("am", 12) => 0,
                ("am", h) => h,
                ("pm", 12) => 12,
                ("pm", h) => h + 12,
                _ => unreachable!(),
            };
            return NaiveTime::from_hms_opt(h24, m, 0);
        }
    }

    // 15:00
    let (h, m) = s.split_once(':')?;
    NaiveTime::from_hms_opt(h.parse().ok()?, m.parse().ok()?, 0)
}

/// "in 3 hours" / "in 90 minutes" — relative to the instant rather than the day.
fn parse_relative_instant(tokens: &[&str], now: DateTime<Local>) -> Option<DateTime<Local>> {
    if tokens.len() != 3 || tokens[0] != "in" {
        return None;
    }
    let n: i64 = tokens[1].parse().ok()?;
    let dur = match tokens[2].trim_end_matches('s') {
        "hour" | "hr" | "h" => Duration::hours(n),
        "minute" | "min" | "m" => Duration::minutes(n),
        _ => return None,
    };
    Some(now + dur)
}

fn parse_date(tokens: &[&str], now: DateTime<Local>) -> Result<Option<NaiveDate>, ParseError> {
    let today = now.date_naive();
    if tokens.is_empty() {
        return Ok(None);
    }

    match tokens {
        ["today"] => return Ok(Some(today)),
        ["tomorrow"] | ["tmr"] | ["tom"] => return Ok(Some(today + Duration::days(1))),
        ["yesterday"] => return Ok(Some(today - Duration::days(1))),
        ["next", "week"] => return Ok(Some(next_week_monday(today))),
        _ => {}
    }

    if let [wd] = tokens {
        if let Some(w) = weekday(wd) {
            return Ok(Some(coming_weekday(today, w)));
        }
    }

    if let ["next", wd] = tokens {
        if let Some(w) = weekday(wd) {
            // "next tuesday" is the Tuesday of next calendar week, never this one.
            return Ok(Some(
                next_week_monday(today) + Duration::days(w.num_days_from_monday() as i64),
            ));
        }
    }

    if let ["in", n, unit] = tokens {
        let n: i64 = n
            .parse()
            .map_err(|_| ParseError(format!("{n:?} is not a number")))?;
        let d = match unit.trim_end_matches('s') {
            "day" | "d" => today + Duration::days(n),
            "week" | "wk" | "w" => today + Duration::weeks(n),
            "month" => add_months(today, n),
            "year" | "yr" => add_months(today, n * 12),
            other => return Err(ParseError(format!("unknown unit {other:?}"))),
        };
        return Ok(Some(d));
    }

    // "dec 25" / "25 dec" / "december 25"
    if let [a, b] = tokens {
        if let (Some(m), Ok(d)) = (
            month(a),
            b.trim_end_matches(&['s', 't', 'h', 'n', 'r'][..])
                .parse::<u32>(),
        ) {
            return Ok(Some(month_day(today, m, d)?));
        }
        if let (Ok(d), Some(m)) = (
            a.trim_end_matches(&['s', 't', 'h', 'n', 'r'][..])
                .parse::<u32>(),
            month(b),
        ) {
            return Ok(Some(month_day(today, m, d)?));
        }
    }

    // "25/12" or "25/12/2026" — day first, matching how the user writes dates.
    if let [one] = tokens {
        let parts: Vec<&str> = one.split(['/', '.']).collect();
        if parts.len() == 2 {
            if let (Ok(d), Ok(m)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
                return Ok(Some(month_day(today, m, d)?));
            }
        }
        if parts.len() == 3 {
            if let (Ok(d), Ok(m), Ok(y)) = (
                parts[0].parse::<u32>(),
                parts[1].parse::<u32>(),
                parts[2].parse::<i32>(),
            ) {
                let y = if y < 100 { 2000 + y } else { y };
                return NaiveDate::from_ymd_opt(y, m, d)
                    .map(Some)
                    .ok_or_else(|| ParseError(format!("no such date: {one}")));
            }
        }
    }

    Err(ParseError(format!(
        "could not understand the date {:?}",
        tokens.join(" ")
    )))
}

/// A month/day with the year inferred: this year if it is still ahead, otherwise next.
fn month_day(today: NaiveDate, m: u32, d: u32) -> Result<NaiveDate, ParseError> {
    let this = NaiveDate::from_ymd_opt(today.year(), m, d);
    match this {
        Some(date) if date >= today => Ok(date),
        _ => NaiveDate::from_ymd_opt(today.year() + 1, m, d)
            .ok_or_else(|| ParseError(format!("no such date: month {m} day {d}"))),
    }
}

/// Clamps to the end of the target month, so 31 Jan + 1 month is 28/29 Feb.
fn add_months(d: NaiveDate, n: i64) -> NaiveDate {
    let total = d.year() as i64 * 12 + (d.month() as i64 - 1) + n;
    let (y, m) = (
        (total.div_euclid(12)) as i32,
        (total.rem_euclid(12) + 1) as u32,
    );
    let last = last_day_of_month(y, m);
    NaiveDate::from_ymd_opt(y, m, d.day().min(last)).expect("clamped day is always valid")
}

fn last_day_of_month(y: i32, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    NaiveDate::from_ymd_opt(ny, nm, 1)
        .expect("valid first of month")
        .pred_opt()
        .expect("valid previous day")
        .day()
}

fn coming_weekday(today: NaiveDate, w: Weekday) -> NaiveDate {
    let ahead = (w.num_days_from_monday() + 7 - today.weekday().num_days_from_monday()) % 7;
    today + Duration::days(ahead as i64)
}

fn next_week_monday(today: NaiveDate) -> NaiveDate {
    today + Duration::days(7 - today.weekday().num_days_from_monday() as i64)
}

fn weekday(s: &str) -> Option<Weekday> {
    Some(match s {
        "mon" | "monday" => Weekday::Mon,
        "tue" | "tues" | "tuesday" => Weekday::Tue,
        "wed" | "weds" | "wednesday" => Weekday::Wed,
        "thu" | "thur" | "thurs" | "thursday" => Weekday::Thu,
        "fri" | "friday" => Weekday::Fri,
        "sat" | "saturday" => Weekday::Sat,
        "sun" | "sunday" => Weekday::Sun,
        _ => return None,
    })
}

fn month(s: &str) -> Option<u32> {
    Some(match s {
        "jan" | "january" => 1,
        "feb" | "february" => 2,
        "mar" | "march" => 3,
        "apr" | "april" => 4,
        "may" => 5,
        "jun" | "june" => 6,
        "jul" | "july" => 7,
        "aug" | "august" => 8,
        "sep" | "sept" | "september" => 9,
        "oct" | "october" => 10,
        "nov" | "november" => 11,
        "dec" | "december" => 12,
        _ => return None,
    })
}

/// "in 2d", "3d ago", "today", "in 4h" — short and dim, for the list view.
pub fn humanize(due: &str, now: DateTime<Local>) -> String {
    let Some(date) = date_part(due) else {
        return due.to_string();
    };

    if has_time(due) {
        if let Some(t) = due
            .split_once('T')
            .and_then(|(_, t)| NaiveTime::parse_from_str(t, "%H:%M").ok())
        {
            let target = date.and_time(t);
            if let Some(target) = Local.from_local_datetime(&target).single() {
                let mins = (target - now).num_minutes();
                if mins == 0 {
                    return "just now".into();
                }
                if mins.abs() < 60 {
                    return if mins > 0 {
                        format!("in {mins}m")
                    } else {
                        format!("{}m ago", -mins)
                    };
                }
                let hours = (target - now).num_hours();
                if hours.abs() < 24 {
                    return if hours >= 0 {
                        format!("in {hours}h")
                    } else {
                        format!("{}h ago", -hours)
                    };
                }
            }
        }
    }

    let days = (date - now.date_naive()).num_days();
    match days {
        0 => "today".into(),
        1 => "tomorrow".into(),
        -1 => "yesterday".into(),
        d if d > 0 && d < 7 => format!("in {d}d"),
        d if d < 0 && d > -7 => format!("{}d ago", -d),
        d if d > 0 && d < 365 => format!("in {}w", d / 7),
        d if d < 0 && d > -365 => format!("{}w ago", -d / 7),
        _ => date.format("%d %b %Y").to_string(),
    }
}

pub fn is_overdue(due: &str, now: DateTime<Local>) -> bool {
    let Some(date) = date_part(due) else {
        return false;
    };
    if has_time(due) {
        if let Some(t) = due
            .split_once('T')
            .and_then(|(_, t)| NaiveTime::parse_from_str(t, "%H:%M").ok())
        {
            if let Some(target) = Local.from_local_datetime(&date.and_time(t)).single() {
                return target < now;
            }
        }
    }
    date < now.date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wednesday, 12 August 2026, 14:30 local. Every test is anchored here.
    fn now() -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 8, 12, 14, 30, 0)
            .single()
            .expect("unambiguous fixture instant")
    }

    fn p(s: &str) -> String {
        parse(s, now())
            .unwrap_or_else(|e| panic!("{s:?} failed: {e}"))
            .expect("a value")
    }

    #[test]
    fn anchor_is_a_wednesday() {
        assert_eq!(now().date_naive().weekday(), Weekday::Wed);
    }

    #[test]
    fn clears() {
        for s in ["none", "clear", "  "] {
            assert_eq!(parse(s, now()).unwrap(), None, "{s:?}");
        }
    }

    #[test]
    fn table_of_every_accepted_form() {
        let cases = [
            // relative days
            ("today", "2026-08-12"),
            ("tomorrow", "2026-08-13"),
            ("yesterday", "2026-08-11"),
            // weekdays: Wednesday anchor
            ("thu", "2026-08-13"),
            ("thursday", "2026-08-13"),
            ("fri", "2026-08-14"),
            ("tue", "2026-08-18"), // already passed this week, so next Tuesday
            ("sun", "2026-08-16"),
            // "next X" is always the following calendar week
            ("next mon", "2026-08-17"),
            ("next wed", "2026-08-19"),
            ("next week", "2026-08-17"),
            // offsets
            ("in 3 days", "2026-08-15"),
            ("in 1 day", "2026-08-13"),
            ("in 2 weeks", "2026-08-26"),
            ("in 1 month", "2026-09-12"),
            // month/day
            ("dec 25", "2026-12-25"),
            ("25 dec", "2026-12-25"),
            ("december 25", "2026-12-25"),
            ("jan 3", "2027-01-03"), // already past this year, so next year
            ("25/12", "2026-12-25"),
            ("25/12/2026", "2026-12-25"),
            ("3/1", "2027-01-03"),
            // ISO
            ("2026-12-25", "2026-12-25"),
        ];
        for (input, want) in cases {
            assert_eq!(p(input), want, "input {input:?}");
        }
    }

    #[test]
    fn table_of_times() {
        let cases = [
            ("friday 3pm", "2026-08-14T15:00"),
            ("friday at 3pm", "2026-08-14T15:00"),
            ("friday 3 pm", "2026-08-14T15:00"),
            ("fri 15:00", "2026-08-14T15:00"),
            ("tomorrow 09:00", "2026-08-13T09:00"),
            ("tomorrow 9am", "2026-08-13T09:00"),
            ("dec 25 noon", "2026-12-25T12:00"),
            ("2026-12-25 15:00", "2026-12-25T15:00"),
            ("2026-12-25T15:00", "2026-12-25T15:00"),
            ("today midnight", "2026-08-12T00:00"),
            ("tomorrow 12:30pm", "2026-08-13T12:30"),
        ];
        for (input, want) in cases {
            assert_eq!(p(input), want, "input {input:?}");
        }
    }

    #[test]
    fn a_weekday_means_today_when_today_is_that_weekday() {
        // The anchor is a Wednesday.
        assert_eq!(p("wed"), "2026-08-12");
        assert_eq!(p("wednesday"), "2026-08-12");
    }

    #[test]
    fn a_bare_time_already_past_means_tomorrow() {
        // Anchor is 14:30.
        assert_eq!(p("3pm"), "2026-08-12T15:00"); // still ahead → today
        assert_eq!(p("2pm"), "2026-08-13T14:00"); // already gone → tomorrow
        assert_eq!(p("14:30"), "2026-08-13T14:30"); // exactly now counts as gone
    }

    #[test]
    fn an_explicit_date_keeps_a_past_time() {
        // "today 9am" is in the past but the user said today, so today it is.
        assert_eq!(p("today 9am"), "2026-08-12T09:00");
    }

    #[test]
    fn relative_instants() {
        assert_eq!(p("in 3 hours"), "2026-08-12T17:30");
        assert_eq!(p("in 90 minutes"), "2026-08-12T16:00");
        // Crossing midnight rolls the date.
        assert_eq!(p("in 10 hours"), "2026-08-13T00:30");
    }

    #[test]
    fn month_arithmetic_clamps_to_the_end_of_the_month() {
        let jan31 = Local
            .with_ymd_and_hms(2026, 1, 31, 9, 0, 0)
            .single()
            .unwrap();
        assert_eq!(parse("in 1 month", jan31).unwrap().unwrap(), "2026-02-28");
        let jan31_leap = Local
            .with_ymd_and_hms(2028, 1, 31, 9, 0, 0)
            .single()
            .unwrap();
        assert_eq!(
            parse("in 1 month", jan31_leap).unwrap().unwrap(),
            "2028-02-29"
        );
    }

    #[test]
    fn leap_day_is_accepted_in_a_leap_year() {
        let anchor = Local
            .with_ymd_and_hms(2028, 1, 1, 9, 0, 0)
            .single()
            .unwrap();
        assert_eq!(parse("feb 29", anchor).unwrap().unwrap(), "2028-02-29");
    }

    #[test]
    fn year_boundary_rolls_forward() {
        let dec30 = Local
            .with_ymd_and_hms(2026, 12, 30, 9, 0, 0)
            .single()
            .unwrap();
        assert_eq!(parse("in 3 days", dec30).unwrap().unwrap(), "2027-01-02");
        assert_eq!(parse("fri", dec30).unwrap().unwrap(), "2027-01-01");
    }

    #[test]
    fn rejects_nonsense() {
        for s in ["blorp", "in 3 fortnights", "32/13", "25pm", "friday 25:00"] {
            assert!(parse(s, now()).is_err(), "{s:?} should not parse");
        }
    }

    #[test]
    fn parse_render_parse_round_trips() {
        for input in [
            "fri",
            "dec 25",
            "tomorrow 9am",
            "2026-12-25T15:00",
            "in 2 weeks",
        ] {
            let once = p(input);
            let twice = p(&once);
            assert_eq!(once, twice, "{input:?} did not round-trip");
        }
    }

    #[test]
    fn humanizes() {
        assert_eq!(humanize("2026-08-12", now()), "today");
        assert_eq!(humanize("2026-08-13", now()), "tomorrow");
        assert_eq!(humanize("2026-08-15", now()), "in 3d");
        assert_eq!(humanize("2026-08-09", now()), "3d ago");
        assert_eq!(humanize("2026-09-12", now()), "in 4w");
    }

    #[test]
    fn overdue_respects_the_time_of_day() {
        assert!(is_overdue("2026-08-11", now()));
        assert!(!is_overdue("2026-08-12", now())); // today is not yet overdue
        assert!(is_overdue("2026-08-12T09:00", now()));
        assert!(!is_overdue("2026-08-12T18:00", now()));
    }
}
