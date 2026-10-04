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
/// The type retains the serial without converting it to a civil date.
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

    /// Returns a fractional day in `[0, 1)`, including for negative serials.
    #[must_use]
    pub fn fractional_day(self) -> f64 {
        self.serial.rem_euclid(1.0)
    }
}
