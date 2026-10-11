//! Static registration schema emitted by the proc-macro façade.
//!
//! This module has no host state or recovery policy. It describes the ABI and
//! metadata that a generated function presents to Excel; preflight and host
//! registration consume these values through the parent registration module.

use crate::{XllError, XllResult};

pub(crate) const MAX_REGISTER_ARGUMENT_HELP_ENTRIES: usize = 244;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Static argument help metadata emitted by the function macro.
pub struct ArgumentDescriptor {
    /// Worksheet-visible argument name.
    pub name: &'static str,
    /// Argument help shown by Excel.
    pub description: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Whether Excel coerces an argument or passes its worksheet reference.
pub enum ArgumentAbi {
    /// Excel supplies a coerced value operand.
    CoercedValue,
    /// Excel supplies a reference operand for an eligible execution mode.
    RawReference,
    /// Excel supplies a contiguous, coerced numeric FP12 array (`K%`).
    NumericArray,
}

pub(crate) use xlfn_common::{ExecutionKind, FunctionVisibility};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RegistrationSignature {
    pub(crate) execution: ExecutionKind,
    pub(crate) arguments: &'static [ArgumentAbi],
    pub(crate) volatile: bool,
}

impl RegistrationSignature {
    pub(crate) fn encode(self) -> XllResult<String> {
        if self.arguments.contains(&ArgumentAbi::RawReference)
            && !self.execution.allows_reference_arguments()
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::REGISTRATION_SIGNATURE,
            });
        }
        let mut text = String::with_capacity(self.arguments.len() + 4);
        text.push(match self.execution {
            ExecutionKind::Async => '>',
            ExecutionKind::MainThread | ExecutionKind::ThreadSafe | ExecutionKind::MacroSheet => {
                'Q'
            }
        });
        for argument in self.arguments {
            text.push_str(match argument {
                ArgumentAbi::CoercedValue => "Q",
                ArgumentAbi::RawReference => "U",
                ArgumentAbi::NumericArray => "K%",
            });
        }
        if self.execution.is_async() {
            text.push('X');
        }
        if matches!(self.execution, ExecutionKind::MacroSheet) {
            text.push('#');
        }
        if self.execution.is_thread_safe() {
            text.push('$');
        }
        if self.volatile {
            text.push('!');
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_arrays_use_one_fp12_argument_with_two_registration_characters() {
        let signature = RegistrationSignature {
            execution: ExecutionKind::ThreadSafe,
            arguments: &[
                ArgumentAbi::NumericArray,
                ArgumentAbi::CoercedValue,
                ArgumentAbi::NumericArray,
            ],
            volatile: false,
        };
        assert_eq!(signature.encode().unwrap(), "QK%QK%$");
    }

    #[test]
    fn fp12_keeps_macro_sheet_and_volatile_registration_flags() {
        let signature = RegistrationSignature {
            execution: ExecutionKind::MacroSheet,
            arguments: &[ArgumentAbi::NumericArray],
            volatile: true,
        };
        assert_eq!(signature.encode().unwrap(), "QK%#!");
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RegistrationDescriptor {
    pub(crate) export_name: &'static str,
    pub(crate) excel_name: &'static str,
    pub(crate) signature: RegistrationSignature,
    pub(crate) category: &'static str,
    pub(crate) description: &'static str,
    pub(crate) help_topic: &'static str,
    pub(crate) visibility: FunctionVisibility,
    pub(crate) arguments: &'static [ArgumentDescriptor],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RegistrationId {
    pub(crate) id: f64,
    pub(crate) excel_name: &'static str,
}
