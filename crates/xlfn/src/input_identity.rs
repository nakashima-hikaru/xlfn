//! Public input-identity API and the optional formula-handle backend.
//!
//! Ordinary input conversions can implement the public identity contracts
//! without pulling their fingerprinting runtime into a feature-off build.

#[cfg(any(feature = "handles", test))]
mod backend;
#[cfg(any(feature = "handles", test))]
pub use backend::InputIdentityEncoder;
#[cfg(any(feature = "handles", test))]
pub(crate) use backend::{InputFingerprint, InputFingerprintBuilder};

#[cfg(not(any(feature = "handles", test)))]
use crate::{XllError, XllResult, error::InputError};

/// Incremental encoder used by generated input-identity implementations.
///
/// The framework creates encoders only for formula-handle calls. The public
/// type remains available so custom input implementations can use the same
/// signatures when the `handles` feature is disabled.
#[cfg(not(any(feature = "handles", test)))]
pub struct InputIdentityEncoder {
    // No safe construction path exists without the formula-handle backend.
    unavailable: std::convert::Infallible,
}

#[cfg(not(any(feature = "handles", test)))]
impl InputIdentityEncoder {
    /// Adds a caller-defined one-byte variant tag.
    pub fn tag(&mut self, _tag: u8) {
        match self.unavailable {}
    }

    /// Adds length-delimited bytes.
    pub fn bytes(&mut self, _bytes: &[u8]) {
        match self.unavailable {}
    }

    /// Adds a UTF-8 string by its bytes, not by its source Excel encoding.
    pub fn string(&mut self, _value: &str) {
        match self.unavailable {}
    }

    /// Adds a boolean value.
    pub fn bool(&mut self, _value: bool) {
        match self.unavailable {}
    }

    /// Adds an `f64` using its converted Rust bit pattern.
    #[inline]
    pub fn f64(&mut self, _value: f64) {
        match self.unavailable {}
    }

    /// Adds a little-endian `u32`.
    pub fn u32(&mut self, _value: u32) {
        match self.unavailable {}
    }

    /// Adds a little-endian `u64`.
    #[inline]
    pub fn u64(&mut self, _value: u64) {
        match self.unavailable {}
    }

    /// Adds a little-endian signed `i64`.
    pub fn i64(&mut self, _value: i64) {
        match self.unavailable {}
    }

    pub(crate) fn semantic_utf16(&mut self, _units: &[u16]) -> XllResult<()> {
        match self.unavailable {}
    }

    pub(crate) fn utf16(&mut self, _units: &[u16]) {
        match self.unavailable {}
    }

    pub(crate) fn f64_sequence(
        &mut self,
        _values: impl Iterator<Item = XllResult<f64>>,
    ) -> XllResult<()> {
        match self.unavailable {}
    }

    pub(crate) const fn argument(&self) -> &'static str {
        match self.unavailable {}
    }

    pub(crate) fn fail(&mut self, _error: XllError) {
        match self.unavailable {}
    }

    pub(crate) fn fail_input(&mut self, _reason: InputError) {
        match self.unavailable {}
    }
}
