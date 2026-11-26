//! When a run started, said as the owner's machine says it: the weekday,
//! the date, the time and the timezone, computed, so an agent never guesses
//! the day of the week.

use jiff::Timestamp;
use jiff::tz::TimeZone;

/// `at` in `zone`, for a run's agent, such as `Thursday 26 November 2026,
/// 06:31 (America/New_York, UTC-05:00)`. A zone with no name is said to be
/// unknown.
#[must_use]
pub fn stamp(at: Timestamp, zone: &TimeZone) -> String {
    let local = at.to_zoned(zone.clone());
    let name = zone.iana_name().unwrap_or("timezone unknown");
    format!(
        "{} ({name}, UTC{})",
        local.strftime("%A %-d %B %Y, %H:%M"),
        local.strftime("%:z")
    )
}

/// `at` in this machine's own timezone; in UTC, said to be unknown, when the
/// machine's can't be read.
#[must_use]
pub fn machine_stamp(at: Timestamp) -> String {
    TimeZone::try_system().map_or_else(
        |_| {
            let utc = at.to_zoned(TimeZone::UTC);
            format!(
                "{} (timezone unknown, UTC{})",
                utc.strftime("%A %-d %B %Y, %H:%M"),
                utc.strftime("%:z")
            )
        },
        |zone| stamp(at, &zone),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> TimeZone {
        jiff::tz::db().get(name).unwrap()
    }

    #[test]
    fn a_stamp_says_the_weekday_date_time_and_zone_as_computed() {
        // 2026-11-26T11:31:00Z.
        let at = Timestamp::from_second(1_795_692_660).unwrap();
        assert_eq!(
            stamp(at, &named("America/New_York")),
            "Thursday 26 November 2026, 06:31 (America/New_York, UTC-05:00)"
        );
        assert_eq!(
            stamp(at, &named("Asia/Tokyo")),
            "Thursday 26 November 2026, 20:31 (Asia/Tokyo, UTC+09:00)"
        );
        // Across midnight, the day is the zone's.
        assert_eq!(
            stamp(at, &named("Pacific/Kiritimati")),
            "Friday 27 November 2026, 01:31 (Pacific/Kiritimati, UTC+14:00)"
        );
        assert_eq!(
            stamp(at, &TimeZone::UTC),
            "Thursday 26 November 2026, 11:31 (UTC, UTC+00:00)"
        );
        let unnamed = TimeZone::fixed(jiff::tz::offset(2));
        assert_eq!(
            stamp(at, &unnamed),
            "Thursday 26 November 2026, 13:31 (timezone unknown, UTC+02:00)"
        );
    }

    #[test]
    fn the_machines_stamp_is_in_its_own_zone() {
        let at = Timestamp::from_second(1_795_692_660).unwrap();
        let said = machine_stamp(at);
        match TimeZone::try_system() {
            Ok(zone) => assert_eq!(said, stamp(at, &zone)),
            Err(_) => assert!(said.contains("(timezone unknown, UTC+00:00)"), "{said}"),
        }
    }
}
