//! Swiss local (civil) time for a given Unix instant (the SNTP-synced clock).
//!
//! Used by the display for the auto-dim window and the clock/date layout elements. Local time
//! = UTC + the Swiss civil offset, computed per-instant with EU daylight-saving rules
//! (CET = UTC+1 in winter, CEST = UTC+2 in summer). Pure functions of the passed-in Unix time —
//! no drawing, no shared state — so the calendar math is reusable and host-testable on its own.

/// Whether `now` (minutes since local midnight) is within the `[start, end)` window, which may
/// wrap past midnight (`start > end`, e.g. 20:00→08:00). An empty `start == end` window never matches.
pub fn in_window(now: u16, start: u16, end: u16) -> bool {
    if start == end {
        false
    } else if start < end {
        now >= start && now < end
    } else {
        now >= start || now < end
    }
}

/// Minutes since local midnight for Unix time `unix` (honouring CET/CEST).
pub fn local_minutes(unix: i64) -> u16 {
    ((unix + swiss_offset_seconds(unix)).rem_euclid(86_400) / 60) as u16
}

/// Local civil parts `(year, month, day, hour, minute)` for Unix time `unix`, for the clock/date
/// elements.
pub fn local_parts(unix: i64) -> (i64, u32, u32, u32, u32) {
    let local = unix + swiss_offset_seconds(unix);
    let (y, m, d) = civil_from_days(local.div_euclid(86_400));
    let sod = local.rem_euclid(86_400);
    (y, m, d, (sod / 3600) as u32, (sod % 3600 / 60) as u32)
}

/// Switzerland's UTC offset (seconds) at Unix time `unix`, honouring EU daylight saving: CEST
/// (UTC+2) from 01:00 UTC on the last Sunday of March to 01:00 UTC on the last Sunday of October,
/// and CET (UTC+1) the rest of the year. Keeps the auto-dim window on wall-clock time year-round
/// (a fixed offset made summer dimming an hour late).
pub fn swiss_offset_seconds(unix: i64) -> i64 {
    let (year, _, _) = civil_from_days(unix.div_euclid(86_400));
    let dst_start = last_sunday_0100_utc(year, 3); // CEST begins
    let dst_end = last_sunday_0100_utc(year, 10); // CET resumes
    if unix >= dst_start && unix < dst_end { 2 * 3600 } else { 3600 }
}

/// Unix seconds for 01:00 UTC on the last Sunday of `month` in `year` — the EU DST switch instant.
/// Both DST months (March, October) have 31 days, so start from the 31st and step back to Sunday.
fn last_sunday_0100_utc(year: i64, month: u32) -> i64 {
    let last = days_from_civil(year, month, 31);
    // 1970-01-01 (day 0) was a Thursday; with 0 = Sunday that is `(days + 4) mod 7`.
    let weekday = (last + 4).rem_euclid(7);
    (last - weekday) * 86_400 + 3600
}

/// Civil date `(year, month, day)` for a count of days since the Unix epoch (Howard Hinnant's
/// algorithm). Only the year is needed here, but the full date keeps the routine self-contained.
/// The maths works on a March-based year (so the leap day is the *last* day of the shifted year),
/// split into 400-year "eras" of exactly 146 097 days.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468; // rebase day 0 from 1970-01-01 to 0000-03-01
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
    let year = year_of_era + era * 400; // March-based year
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let month_from_march = (5 * day_of_year + 2) / 153; // [0, 11]: 0 = March
    let day = (day_of_year - (153 * month_from_march + 2) / 5 + 1) as u32; // [1, 31]
    let month =
        if month_from_march < 10 { month_from_march + 3 } else { month_from_march - 9 } as u32; // [1, 12]
    (year + if month <= 2 { 1 } else { 0 }, month, day)
}

/// Days since the Unix epoch for civil date `(year, month, day)` (inverse of [`civil_from_days`]).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year }; // March-based year (see civil_from_days)
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400; // [0, 399]
    let month = month as i64;
    let day_of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day as i64 - 1; // [0, 365]
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year; // [0, 146096]
    era * 146_097 + day_of_era - 719_468
}
