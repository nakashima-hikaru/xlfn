//! Static registration schema emitted by the proc-macro façade.
//!
//! This module has no host state or recovery policy. It describes the ABI and
//! metadata that a generated function presents to Excel; preflight and host
//! registration consume these values through the parent registration module.

use crate::{XllError, XllResult};

pub const MAX_EXCEL_FUNCTION_ARGUMENTS: usize = 255;
pub const MAX_REGISTER_ARGUMENT_HELP_ENTRIES: usize = 244;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArgumentDescriptor {
    pub name: &'static str,
    pub description: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArgumentAbi {
    CoercedValue,
    RawReference,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResultAbi {
    Xloper,
    AsyncVoid,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RegistrationFlags {
    pub thread_safe: bool,
    pub macro_sheet: bool,
    pub volatile: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FunctionVisibility {
    #[default]
    Public,
    Hidden,
}

impl FunctionVisibility {
    pub(crate) const fn macro_type(self) -> f64 {
        match self {
            Self::Public => 1.0,
            Self::Hidden => 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegistrationSignature {
    pub result: ResultAbi,
    pub arguments: &'static [ArgumentAbi],
    pub flags: RegistrationFlags,
}

impl RegistrationSignature {
    pub fn encode(self) -> XllResult<String> {
        if self.flags.thread_safe && self.flags.macro_sheet
            || self.arguments.contains(&ArgumentAbi::RawReference) && !self.flags.macro_sheet
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::REGISTRATION_SIGNATURE,
            });
        }
        let mut text = String::with_capacity(self.arguments.len() + 4);
        text.push(match self.result {
            ResultAbi::Xloper => 'Q',
            ResultAbi::AsyncVoid => '>',
        });
        for argument in self.arguments {
            text.push(match argument {
                ArgumentAbi::CoercedValue => 'Q',
                ArgumentAbi::RawReference => 'U',
            });
        }
        if self.result == ResultAbi::AsyncVoid {
            text.push('X');
        }
        if self.flags.macro_sheet {
            text.push('#');
        }
        if self.flags.thread_safe {
            text.push('$');
        }
        if self.flags.volatile {
            text.push('!');
        }
        Ok(text)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RegistrationDescriptor {
    pub export_name: &'static str,
    pub excel_name: &'static str,
    pub signature: RegistrationSignature,
    pub category: &'static str,
    pub description: &'static str,
    pub help_topic: &'static str,
    pub visibility: FunctionVisibility,
    pub arguments: &'static [ArgumentDescriptor],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegistrationId {
    pub id: f64,
    pub excel_name: &'static str,
}
