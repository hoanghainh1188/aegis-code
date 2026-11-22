//! Instants as the API and the dashboard write them (#578), all in UTC.

use time::OffsetDateTime;
use time::format_description::BorrowedFormatItem;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;

const SECOND: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
const MINUTE: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day] [hour]:[minute]");
const DAY: &[BorrowedFormatItem<'_>] = format_description!("[year]-[month]-[day]");
const MONTH: &[BorrowedFormatItem<'_>] = format_description!("[year]-[month]");
/// The ISO week, as `2026-W47`: the year the week belongs to, which can
/// differ from the date's own near New Year.
const ISO_WEEK: &[BorrowedFormatItem<'_>] =
    format_description!("[year repr:full base:iso_week]-W[week_number repr:iso]");

/// `t` with `format`; one that cannot be written (a year past 9999)
/// falls back to its debug form rather than failing the page.
fn written(t: OffsetDateTime, format: &(impl time::formatting::Formattable + ?Sized)) -> String {
    let t = t.to_offset(time::UtcOffset::UTC);
    t.format(format).unwrap_or_else(|_| t.to_string())
}

/// RFC 3339, as the API returns times: `2026-11-22T10:00:00.123456Z`.
pub fn rfc3339(t: OffsetDateTime) -> String {
    written(t, &Rfc3339)
}

/// `2026-11-22 10:00:05`.
pub fn second(t: OffsetDateTime) -> String {
    written(t, SECOND)
}

/// `2026-11-22 10:00`.
pub fn minute(t: OffsetDateTime) -> String {
    written(t, MINUTE)
}

/// `2026-11-22`.
pub fn day(t: OffsetDateTime) -> String {
    written(t, DAY)
}

/// `2026-11`.
pub fn month(t: OffsetDateTime) -> String {
    written(t, MONTH)
}

/// `2026-W47`.
pub fn iso_week(t: OffsetDateTime) -> String {
    written(t, ISO_WEEK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn instants_are_written_in_utc() {
        let t = datetime!(2026-11-22 17:00:05.123456 +07:00);
        assert_eq!(rfc3339(t), "2026-11-22T10:00:05.123456Z");
        assert_eq!(second(t), "2026-11-22 10:00:05");
        assert_eq!(minute(t), "2026-11-22 10:00");
        assert_eq!(day(t), "2026-11-22");
        assert_eq!(month(t), "2026-11");
        assert_eq!(iso_week(t), "2026-W47");
    }

    /// Like chrono's `%G-W%V`: 1 January 2027 is in 2026's last week.
    #[test]
    fn the_iso_week_takes_the_week_s_year() {
        assert_eq!(iso_week(datetime!(2027-01-01 12:00 UTC)), "2026-W53");
        assert_eq!(iso_week(datetime!(2025-12-29 12:00 UTC)), "2026-W01");
    }
}
