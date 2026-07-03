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
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

/// Days since the Unix epoch for civil date `(year, month, day)` (inverse of [`civil_from_days`]).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400; // [0, 399]
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}
