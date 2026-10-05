//! Excel serial-date policy and semantic values.

use crate::error::InputError;
use crate::{XllError, XllResult};

/// The workbook convention used to interpret an Excel serial date.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ExcelDateSystem {
    /// The workbook setting has not yet been resolved by the caller.
    #[default]
    Workbook,
    /// The 1900 system, including Excel's fictitious 1900-02-29.
    Windows1900,
    /// The 1904 system, whose serial zero is 1904-01-01.
    Mac1904,
}

/// A finite Excel serial paired with an explicit or unresolved date system.
///
/// The type retains the serial without converting it to a civil date. Calendar
/// conversion requires an explicit date system and supports Gregorian years
/// `1..=9999`; it rejects Excel's fictitious 1900-02-29.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExcelSerialDate {
    pub(super) serial: f64,
    pub(super) date_system: ExcelDateSystem,
}

impl ExcelSerialDate {
    /// Creates a serial date, rejecting non-finite numbers.
    pub fn new(serial: f64, date_system: ExcelDateSystem) -> XllResult<Self> {
        if !serial.is_finite() {
            return Err(XllError::input("date", InputError::NonFinite));
        }
        Ok(Self {
            serial,
            date_system,
        })
    }

    /// Creates a midnight serial from a Gregorian year, month, and day.
    ///
    /// Years must be in `1..=9999`, and the date must exist in the Gregorian
    /// calendar. `Workbook` is rejected because its epoch is unresolved.
    /// Dates preceding the selected epoch produce negative serials.
    pub fn from_ymd(
        year: i32,
        month: u32,
        day: u32,
        date_system: ExcelDateSystem,
    ) -> XllResult<Self> {
        let epoch = date_epoch(date_system)?;
        if !(1..=9999).contains(&year)
            || !(1..=12).contains(&month)
            || day == 0
            || day > days_in_month(year, month)
        {
            return Err(date_out_of_range());
        }
        let ordinal = days_before_year(year)
            + DAYS_BEFORE_MONTH[month as usize - 1]
            + i32::from(month > 2 && is_leap_year(year))
            + day as i32
            - 1;
        let fictitious_day =
            i32::from(date_system == ExcelDateSystem::Windows1900 && ordinal >= MARCH_1900);
        Self::new(f64::from(ordinal - epoch + fictitious_day), date_system)
    }

    /// Returns the Gregorian year, month, and day of the serial's floor.
    ///
    /// The fractional day is ignored, including for negative serials: `-0.25`
    /// belongs to the day preceding serial zero. An unresolved `Workbook`
    /// convention, the fictitious 1900 leap day, and dates outside years
    /// `1..=9999` return an error.
    pub fn to_ymd(self) -> XllResult<(i32, u32, u32)> {
        let epoch = date_epoch(self.date_system)?;
        let serial_day = self.serial.floor();
        if self.is_fictitious_1900_leap_day() {
            return Err(XllError::input(
                "date",
                InputError::Malformed("1900-02-29 is not a Gregorian date"),
            ));
        }
        let fictitious_day =
            i32::from(self.date_system == ExcelDateSystem::Windows1900 && serial_day >= 61.0);
        let ordinal = serial_day + f64::from(epoch - fictitious_day);
        if !(0.0..f64::from(days_before_year(10000))).contains(&ordinal) {
            return Err(date_out_of_range());
        }
        let ordinal = ordinal as i32;
        // Find the containing year without narrowing an unchecked serial or
        // depending on a platform-sized integer.
        let mut year = 1;
        let mut end = 10000;
        while year + 1 < end {
            let middle = year + (end - year) / 2;
            if days_before_year(middle) <= ordinal {
                year = middle;
            } else {
                end = middle;
            }
        }
        let mut day = (ordinal - days_before_year(year)) as u32;
        let mut month = 1;
        while day >= days_in_month(year, month) {
            day -= days_in_month(year, month);
            month += 1;
        }
        Ok((year, month, day + 1))
    }

    /// Creates a midnight serial from a `chrono` calendar date.
    ///
    /// This requires the `chrono` feature. The same explicit date-system and
    /// year-range rules as [`Self::from_ymd`] apply.
    #[cfg(feature = "chrono")]
    pub fn from_chrono(date: chrono::NaiveDate, date_system: ExcelDateSystem) -> XllResult<Self> {
        use chrono::Datelike;
        Self::from_ymd(date.year(), date.month(), date.day(), date_system)
    }

    /// Converts the serial's calendar day to a `chrono` date.
    ///
    /// This requires the `chrono` feature. The fractional day is discarded,
    /// and the same failure rules as [`Self::to_ymd`] apply.
    #[cfg(feature = "chrono")]
    pub fn to_chrono(self) -> XllResult<chrono::NaiveDate> {
        let (year, month, day) = self.to_ymd()?;
        chrono::NaiveDate::from_ymd_opt(year, month, day).ok_or_else(date_out_of_range)
    }

    /// Creates a midnight serial from a `time` calendar date.
    ///
    /// This requires the `time` feature. The same explicit date-system and
    /// year-range rules as [`Self::from_ymd`] apply.
    #[cfg(feature = "time")]
    pub fn from_time(date: time::Date, date_system: ExcelDateSystem) -> XllResult<Self> {
        let (year, month, day) = date.to_calendar_date();
        Self::from_ymd(
            year,
            u32::from(u8::from(month)),
            u32::from(day),
            date_system,
        )
    }

    /// Converts the serial's calendar day to a `time` date.
    ///
    /// This requires the `time` feature. The fractional day is discarded,
    /// and the same failure rules as [`Self::to_ymd`] apply.
    #[cfg(feature = "time")]
    pub fn to_time(self) -> XllResult<time::Date> {
        let (year, month, day) = self.to_ymd()?;
        let month = time::Month::try_from(month as u8).map_err(|_| date_out_of_range())?;
        time::Date::from_calendar_date(year, month, day as u8).map_err(|_| date_out_of_range())
    }

    /// Returns the original finite serial, including its fractional day.
    #[must_use]
    pub const fn serial(self) -> f64 {
        self.serial
    }

    /// Returns the date convention, which may still be unresolved.
    #[must_use]
    pub const fn date_system(self) -> ExcelDateSystem {
        self.date_system
    }

    /// Attaches a date convention without changing the serial.
    #[must_use]
    pub const fn with_date_system(mut self, date_system: ExcelDateSystem) -> Self {
        self.date_system = date_system;
        self
    }

    /// Detects the fictitious leap day in the 1900 system (serial day 60).
    #[must_use]
    pub fn is_fictitious_1900_leap_day(self) -> bool {
        self.date_system == ExcelDateSystem::Windows1900 && self.serial.floor() == 60.0
    }

    /// Returns the Euclidean remainder of the serial modulo one day.
    ///
    /// The result is normally in `[0, 1)`, including for negative serials.
    /// Floating-point rounding can yield `1.0` for a small negative serial.
    #[must_use]
    pub fn fractional_day(self) -> f64 {
        self.serial.rem_euclid(1.0)
    }
}

const DAYS_BEFORE_MONTH: [i32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
const MARCH_1900: i32 = days_before_year(1900) + 59;

const fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

const fn days_before_year(year: i32) -> i32 {
    let previous = year - 1;
    365 * previous + previous / 4 - previous / 100 + previous / 400
}

const fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        2 if is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn date_epoch(date_system: ExcelDateSystem) -> XllResult<i32> {
    match date_system {
        ExcelDateSystem::Windows1900 => Ok(days_before_year(1900) - 1),
        ExcelDateSystem::Mac1904 => Ok(days_before_year(1904)),
        ExcelDateSystem::Workbook => Err(XllError::input(
            "date",
            InputError::Malformed("resolve the workbook date system before calendar conversion"),
        )),
    }
}

fn date_out_of_range() -> XllError {
    XllError::input("date", InputError::OutOfRange)
}

#[cfg(test)]
mod tests {
    use super::{ExcelDateSystem, ExcelSerialDate};

    #[test]
    fn known_calendar_dates_cover_both_epochs_and_the_1900_gap() {
        for (system, dates) in [
            (
                ExcelDateSystem::Windows1900,
                [
                    ((1899, 12, 30), -1.0),
                    ((1899, 12, 31), 0.0),
                    ((1900, 1, 1), 1.0),
                    ((1900, 2, 28), 59.0),
                    ((1900, 3, 1), 61.0),
                    ((1904, 1, 1), 1462.0),
                    ((2026, 10, 5), 46300.0),
                ],
            ),
            (
                ExcelDateSystem::Mac1904,
                [
                    ((1899, 12, 30), -1462.0),
                    ((1899, 12, 31), -1461.0),
                    ((1900, 1, 1), -1460.0),
                    ((1900, 2, 28), -1402.0),
                    ((1900, 3, 1), -1401.0),
                    ((1904, 1, 1), 0.0),
                    ((2026, 10, 5), 44838.0),
                ],
            ),
        ] {
            for ((year, month, day), serial) in dates {
                let date = ExcelSerialDate::from_ymd(year, month, day, system).unwrap();
                assert_eq!(date.serial(), serial);
                assert_eq!(date.to_ymd().unwrap(), (year, month, day));
                assert_eq!(date.date_system(), system);
            }
        }
    }

    #[test]
    fn calendar_conversion_rejects_unresolved_and_fictitious_dates() {
        assert!(ExcelSerialDate::from_ymd(2026, 10, 5, ExcelDateSystem::Workbook).is_err());
        assert!(
            ExcelSerialDate::new(46300.0, ExcelDateSystem::Workbook)
                .unwrap()
                .to_ymd()
                .is_err()
        );
        for serial in [60.0, 60.5, 60.999_999] {
            let fake = ExcelSerialDate::new(serial, ExcelDateSystem::Windows1900).unwrap();
            assert!(fake.is_fictitious_1900_leap_day());
            assert!(fake.to_ymd().is_err());
        }
        assert!(ExcelSerialDate::from_ymd(1900, 2, 29, ExcelDateSystem::Windows1900).is_err());
        assert_eq!(
            ExcelSerialDate::new(60.5, ExcelDateSystem::Mac1904)
                .unwrap()
                .to_ymd()
                .unwrap(),
            (1904, 3, 1)
        );
    }

    #[test]
    fn calendar_conversion_checks_dates_and_year_bounds_before_arithmetic() {
        for system in [ExcelDateSystem::Windows1900, ExcelDateSystem::Mac1904] {
            for (year, month, day) in [
                (i32::MIN, 1, 1),
                (i32::MAX, 1, 1),
                (0, 1, 1),
                (10000, 1, 1),
                (2026, 0, 1),
                (2026, u32::MAX, 1),
                (2026, 1, 0),
                (2026, 1, u32::MAX),
                (2026, 4, 31),
                (1900, 2, 29),
                (2100, 2, 29),
            ] {
                assert!(ExcelSerialDate::from_ymd(year, month, day, system).is_err());
            }
            assert_eq!(
                ExcelSerialDate::from_ymd(2000, 2, 29, system)
                    .unwrap()
                    .to_ymd()
                    .unwrap(),
                (2000, 2, 29)
            );
            let minimum = ExcelSerialDate::from_ymd(1, 1, 1, system).unwrap();
            let maximum = ExcelSerialDate::from_ymd(9999, 12, 31, system).unwrap();
            assert_eq!(minimum.to_ymd().unwrap(), (1, 1, 1));
            assert_eq!(maximum.to_ymd().unwrap(), (9999, 12, 31));
            for serial in [
                minimum.serial() - 1.0,
                maximum.serial() + 1.0,
                f64::MIN,
                f64::MAX,
            ] {
                assert!(
                    ExcelSerialDate::new(serial, system)
                        .unwrap()
                        .to_ymd()
                        .is_err()
                );
            }
            assert_eq!(
                ExcelSerialDate::new(maximum.serial() + 0.75, system)
                    .unwrap()
                    .to_ymd()
                    .unwrap(),
                (9999, 12, 31)
            );
        }
    }

    #[test]
    fn fractional_serials_use_floor_even_near_negative_zero() {
        for serial in [-0.25, -f64::EPSILON / 4.0] {
            let date = ExcelSerialDate::new(serial, ExcelDateSystem::Windows1900).unwrap();
            assert_eq!(date.to_ymd().unwrap(), (1899, 12, 30));
        }
        assert_eq!(
            ExcelSerialDate::new(-f64::EPSILON / 4.0, ExcelDateSystem::Windows1900)
                .unwrap()
                .fractional_day(),
            1.0
        );
        assert_eq!(
            ExcelSerialDate::new(61.75, ExcelDateSystem::Windows1900)
                .unwrap()
                .to_ymd()
                .unwrap(),
            (1900, 3, 1)
        );
    }

    #[cfg(feature = "chrono")]
    #[test]
    fn chrono_conversion_matches_independent_calendar_arithmetic() {
        use chrono::NaiveDate;

        let windows_epoch = NaiveDate::from_ymd_opt(1899, 12, 31).unwrap();
        let mac_epoch = NaiveDate::from_ymd_opt(1904, 1, 1).unwrap();
        let march_1900 = NaiveDate::from_ymd_opt(1900, 3, 1).unwrap();
        for year in [
            1, 4, 99, 100, 400, 1600, 1700, 1899, 1900, 1904, 2000, 2026, 2100, 9999,
        ] {
            for month in 1..=12 {
                for day in 1..=31 {
                    let Some(calendar) = NaiveDate::from_ymd_opt(year, month, day) else {
                        continue;
                    };
                    for (system, epoch) in [
                        (ExcelDateSystem::Windows1900, windows_epoch),
                        (ExcelDateSystem::Mac1904, mac_epoch),
                    ] {
                        let expected = calendar.signed_duration_since(epoch).num_days()
                            + i64::from(
                                system == ExcelDateSystem::Windows1900 && calendar >= march_1900,
                            );
                        let date = ExcelSerialDate::from_chrono(calendar, system).unwrap();
                        assert_eq!(date.serial(), expected as f64);
                        assert_eq!(date.to_chrono().unwrap(), calendar);
                    }
                }
            }
        }
        for year in [0, -1, 10000] {
            let calendar = NaiveDate::from_ymd_opt(year, 1, 1).unwrap();
            assert!(ExcelSerialDate::from_chrono(calendar, ExcelDateSystem::Windows1900).is_err());
        }
        assert!(ExcelSerialDate::from_chrono(windows_epoch, ExcelDateSystem::Workbook).is_err());
        for system in [ExcelDateSystem::Windows1900, ExcelDateSystem::Workbook] {
            assert!(
                ExcelSerialDate::new(60.5, system)
                    .unwrap()
                    .to_chrono()
                    .is_err()
            );
        }
    }

    #[cfg(feature = "time")]
    #[test]
    fn time_conversion_preserves_dates_and_rejects_unrepresentable_inputs() {
        use time::{Date, Month};

        for (year, month, day) in [
            (1, Month::January, 1),
            (1900, Month::February, 28),
            (1900, Month::March, 1),
            (1904, Month::February, 29),
            (2000, Month::February, 29),
            (2026, Month::October, 5),
            (9999, Month::December, 31),
        ] {
            let calendar = Date::from_calendar_date(year, month, day).unwrap();
            for system in [ExcelDateSystem::Windows1900, ExcelDateSystem::Mac1904] {
                let date = ExcelSerialDate::from_time(calendar, system).unwrap();
                assert_eq!(date.to_time().unwrap(), calendar);
            }
            assert!(ExcelSerialDate::from_time(calendar, ExcelDateSystem::Workbook).is_err());
        }
        for year in [-1, 0] {
            let calendar = Date::from_calendar_date(year, Month::January, 1).unwrap();
            assert!(ExcelSerialDate::from_time(calendar, ExcelDateSystem::Mac1904).is_err());
        }
        for system in [ExcelDateSystem::Windows1900, ExcelDateSystem::Workbook] {
            assert!(
                ExcelSerialDate::new(60.5, system)
                    .unwrap()
                    .to_time()
                    .is_err()
            );
        }
    }
}
