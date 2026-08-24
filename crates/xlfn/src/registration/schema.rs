//! Static registration schema emitted by the proc-macro façade.
//!
//! This module has no host state or recovery policy. It describes the ABI and
//! metadata that a generated function presents to Excel; preflight and host
//! registration consume these values through the parent registration module.

use crate::{XllError, XllResult};

pub(crate) const MAX_REGISTER_ARGUMENT_HELP_ENTRIES: usize = 244;

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
            text.push(match argument {
                ArgumentAbi::CoercedValue => 'Q',
                ArgumentAbi::RawReference => 'U',
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
