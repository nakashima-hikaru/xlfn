//! Portable value descriptions; no pointers or native ABI state.

/// The number of rows and columns in an Excel array or Rust matrix.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Shape {
    /// The number of rows.
    pub rows: usize,
    /// The number of columns.
    pub columns: usize,
}

/// The validated semantic value type of an Excel `XLOPER12`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum XlValueType {
    /// A binary64 number.
    Number,
    /// A counted UTF-16 string.
    String,
    /// A Boolean.
    Boolean,
    /// A worksheet reference that may contain multiple areas.
    Reference,
    /// An Excel error code.
    Error,
    /// An Excel control-flow value.
    Flow,
    /// A rectangular array of cells.
    Multi,
    /// An omitted argument.
    Missing,
    /// A blank cell.
    Nil,
    /// A single-area worksheet reference.
    SimpleReference,
    /// A signed 32-bit integer.
    Integer,
    /// An Excel binary-data value.
    BigData,
}

impl std::fmt::Display for XlValueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number => write!(f, "Number"),
            Self::String => write!(f, "String"),
            Self::Boolean => write!(f, "Boolean"),
            Self::Reference => write!(f, "Reference"),
            Self::Error => write!(f, "Error"),
            Self::Flow => write!(f, "Flow"),
            Self::Multi => write!(f, "Multi"),
            Self::Missing => write!(f, "Missing"),
            Self::Nil => write!(f, "Nil"),
            Self::SimpleReference => write!(f, "SimpleReference"),
            Self::Integer => write!(f, "Integer"),
            Self::BigData => write!(f, "BigData"),
        }
    }
}
