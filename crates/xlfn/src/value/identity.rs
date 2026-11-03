//! Canonical identity composition for fields of custom derived inputs.

use super::{
    Column, ExcelInputIdentity, InputIdentityEncoder, Matrix, MatrixRef, OptionalExcelValue, Row,
};

impl<T: ExcelInputIdentity> ExcelInputIdentity for Matrix<T> {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.u64(self.rows() as u64);
        encoder.u64(self.columns() as u64);
        for value in self.as_slice() {
            value.encode_input_identity(encoder);
        }
    }
}
impl<T: ExcelInputIdentity> ExcelInputIdentity for MatrixRef<'_, T> {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.u64(self.rows() as u64);
        encoder.u64(self.columns() as u64);
        for value in self.as_slice() {
            value.encode_input_identity(encoder);
        }
    }
}
impl<T: ExcelInputIdentity> ExcelInputIdentity for Vec<T> {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.u64(self.len() as u64);
        for value in self {
            value.encode_input_identity(encoder);
        }
    }
}
macro_rules! sequence_identity {
    ($ty:ident) => {
        impl<T: ExcelInputIdentity> ExcelInputIdentity for $ty<T> {
            fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
                encoder.u64(self.as_slice().len() as u64);
                for value in self.as_slice() {
                    value.encode_input_identity(encoder);
                }
            }
        }
    };
}
sequence_identity!(Row);
sequence_identity!(Column);
impl<T: ExcelInputIdentity> ExcelInputIdentity for Option<T> {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.bool(self.is_some());
        if let Some(value) = self {
            value.encode_input_identity(encoder);
        }
    }
}
impl<T: ExcelInputIdentity> ExcelInputIdentity for OptionalExcelValue<T> {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        match self {
            Self::Missing => encoder.tag(0),
            Self::Blank => encoder.tag(1),
            Self::Value(value) => {
                encoder.tag(2);
                value.encode_input_identity(encoder);
            }
        }
    }
}
