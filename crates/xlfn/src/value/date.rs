//! Excel serial-date policy and semantic values.

use crate::{InputError, XllError, XllResult};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ExcelDateSystem {
    /// The workbook setting has not yet been resolved by the caller.
    #[default]
    Workbook,
    Windows1900,
    Mac1904,
}

impl ExcelDateSystem {}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExcelSerialDate {
    pub(super) serial: f64,
    pub(super) date_system: ExcelDateSystem,
}

impl ExcelSerialDate {
    pub fn new(serial: f64, date_system: ExcelDateSystem) -> XllResult<Self> {
        if !serial.is_finite() {
            return Err(XllError::input("date", InputError::NonFinite));
        }
        Ok(Self {
            serial,
            date_system,
        })
    }

    #[must_use]
    pub const fn serial(self) -> f64 {
        self.serial
    }

    #[must_use]
    pub const fn date_system(self) -> ExcelDateSystem {
        self.date_system
    }

    #[must_use]
    pub const fn with_date_system(mut self, date_system: ExcelDateSystem) -> Self {
        self.date_system = date_system;
        self
    }

    #[must_use]
    pub fn is_fictitious_1900_leap_day(self) -> bool {
        self.date_system == ExcelDateSystem::Windows1900 && self.serial.floor() == 60.0
    }

    #[must_use]
    pub fn fractional_day(self) -> f64 {
        self.serial.rem_euclid(1.0)
    }
}
