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

pub use xlfn_common::{ArgumentAbi, ReturnAbi};

pub(crate) use xlfn_common::{ExecutionKind, FunctionVisibility};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RegistrationSignature {
    pub(crate) execution: ExecutionKind,
    pub(crate) arguments: &'static [ArgumentAbi],
    pub(crate) return_abi: ReturnAbi,
    pub(crate) volatile: bool,
}

impl RegistrationSignature {
    pub(crate) fn validate(self) -> XllResult<()> {
        if self.arguments.len() > xlfn_common::max_excel_function_arguments(self.execution)
            || (self.arguments.contains(&ArgumentAbi::Reference12)
                && !self.execution.allows_reference_arguments())
            || (self.arguments.contains(&ArgumentAbi::NumericArray12) && self.execution.is_async())
            || (self.return_abi == ReturnAbi::NumericArray12 && self.execution.is_async())
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::REGISTRATION_SIGNATURE,
            });
        }
        Ok(())
    }

    pub(crate) fn encode(self) -> XllResult<String> {
        self.validate()?;
        let mut text = String::with_capacity(self.arguments.len() + 4);
        if self.execution.is_async() {
            text.push('>');
        } else {
            text.push_str(self.return_abi.registration_code());
        }
        for argument in self.arguments {
            text.push_str(argument.registration_code());
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
    fn native_scalars_share_the_canonical_registration_codes() {
        const INPUTS: &[ArgumentAbi] = &[
            ArgumentAbi::Float64,
            ArgumentAbi::Boolean16,
            ArgumentAbi::Integer32,
            ArgumentAbi::CountedUtf16,
        ];
        for (execution, expected) in [
            (ExecutionKind::ThreadSafe, "QBAJD%$"),
            (ExecutionKind::MainThread, "QBAJD%"),
            (ExecutionKind::Async, ">BAJD%X$"),
        ] {
            assert_eq!(
                RegistrationSignature {
                    return_abi: crate::registration::ReturnAbi::Value12,
                    execution,
                    arguments: INPUTS,
                    volatile: false,
                }
                .encode()
                .unwrap(),
                expected,
            );
        }
        assert_eq!(
            RegistrationSignature {
                return_abi: crate::registration::ReturnAbi::Value12,
                execution: ExecutionKind::ThreadSafe,
                arguments: &[
                    ArgumentAbi::Value12,
                    ArgumentAbi::Boolean16,
                    ArgumentAbi::Value12
                ],
                volatile: false,
            }
            .encode()
            .unwrap(),
            "QQAQ$",
        );
    }

    #[test]
    fn numeric_arrays_use_one_fp12_argument_with_two_registration_characters() {
        let signature = RegistrationSignature {
            return_abi: crate::registration::ReturnAbi::Value12,
            execution: ExecutionKind::ThreadSafe,
            arguments: &[
                ArgumentAbi::NumericArray12,
                ArgumentAbi::Value12,
                ArgumentAbi::NumericArray12,
            ],
            volatile: false,
        };
        assert_eq!(signature.encode().unwrap(), "QK%QK%$");
    }

    #[test]
    fn numeric_results_encode_the_same_abi_as_the_generated_fp12_entrypoint() {
        let signature = RegistrationSignature {
            execution: ExecutionKind::ThreadSafe,
            arguments: &[ArgumentAbi::Float64, ArgumentAbi::NumericArray12],
            return_abi: ReturnAbi::NumericArray12,
            volatile: false,
        };
        assert_eq!(signature.encode().unwrap(), "K%BK%$");
        let explicit_value = RegistrationSignature {
            return_abi: ReturnAbi::Value12,
            ..signature
        };
        assert_eq!(explicit_value.encode().unwrap(), "QBK%$");
        let macro_sheet = RegistrationSignature {
            execution: ExecutionKind::MacroSheet,
            volatile: true,
            ..signature
        };
        assert_eq!(macro_sheet.encode().unwrap(), "K%BK%#!");
    }

    #[test]
    fn fp12_keeps_macro_sheet_and_volatile_registration_flags() {
        let signature = RegistrationSignature {
            return_abi: crate::registration::ReturnAbi::Value12,
            execution: ExecutionKind::MacroSheet,
            arguments: &[ArgumentAbi::NumericArray12],
            volatile: true,
        };
        assert_eq!(signature.encode().unwrap(), "QK%#!");
    }

    #[test]
    fn signatures_reject_unsupported_async_inputs_and_argument_counts() {
        for signature in [
            RegistrationSignature {
                return_abi: ReturnAbi::NumericArray12,
                execution: ExecutionKind::Async,
                arguments: &[],
                volatile: false,
            },
            RegistrationSignature {
                return_abi: crate::registration::ReturnAbi::Value12,
                execution: ExecutionKind::Async,
                arguments: &[ArgumentAbi::NumericArray12],
                volatile: false,
            },
            RegistrationSignature {
                return_abi: crate::registration::ReturnAbi::Value12,
                execution: ExecutionKind::ThreadSafe,
                arguments: &[ArgumentAbi::Reference12],
                volatile: false,
            },
            RegistrationSignature {
                return_abi: crate::registration::ReturnAbi::Value12,
                execution: ExecutionKind::MainThread,
                arguments: &[ArgumentAbi::Value12; 256],
                volatile: false,
            },
            RegistrationSignature {
                return_abi: crate::registration::ReturnAbi::Value12,
                execution: ExecutionKind::Async,
                arguments: &[ArgumentAbi::Value12; 255],
                volatile: false,
            },
        ] {
            assert!(matches!(
                signature.encode(),
                Err(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::REGISTRATION_SIGNATURE,
                })
            ));
        }
    }

    #[test]
    fn argument_limit_counts_fp12_arguments_instead_of_type_code_characters() {
        // Excel12 counted Unicode strings support 32,767 code units. K% is
        // one worksheet argument even though its type code uses two units.
        let signature = RegistrationSignature {
            return_abi: crate::registration::ReturnAbi::Value12,
            execution: ExecutionKind::ThreadSafe,
            arguments: &[ArgumentAbi::NumericArray12; 255],
            volatile: true,
        };
        let encoded = signature.encode().unwrap();
        assert_eq!(encoded, format!("Q{}$!", "K%".repeat(255)));
        assert!(xlfn_common::validate_excel_string(&encoded).is_ok());

        let asynchronous = RegistrationSignature {
            return_abi: crate::registration::ReturnAbi::Value12,
            execution: ExecutionKind::Async,
            arguments: &[ArgumentAbi::Value12; 254],
            volatile: true,
        };
        assert_eq!(
            asynchronous.encode().unwrap(),
            format!(">{}X$!", "Q".repeat(254))
        );
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
