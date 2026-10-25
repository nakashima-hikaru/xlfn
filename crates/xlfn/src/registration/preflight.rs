//! Pure registration validation and host-independent preparation.

use super::schema::MAX_REGISTER_ARGUMENT_HELP_ENTRIES;
use super::{ExcelNameKey, RegistrationDescriptor, RegistrationSignature};
use crate::{XllError, XllResult};
use rustc_hash::FxHashSet;
use smallvec::SmallVec;
use std::borrow::Cow;

pub(crate) fn validate_descriptors(descriptors: &[RegistrationDescriptor]) -> XllResult<()> {
    let mut exports = FxHashSet::with_capacity_and_hasher(descriptors.len(), Default::default());
    let mut excel_names =
        FxHashSet::with_capacity_and_hasher(descriptors.len(), Default::default());
    for descriptor in descriptors {
        let max_arguments =
            xlfn_common::max_excel_function_arguments(descriptor.signature.execution);
        let argument_names = descriptor
            .arguments
            .iter()
            .map(|argument| argument.name)
            .collect::<SmallVec<[&str; 16]>>();
        if descriptor.export_name.is_empty()
            || xlfn_common::validate_function_name(descriptor.excel_name).is_err()
            || descriptor.arguments.len() > max_arguments
            || xlfn_common::validate_argument_names(&argument_names).is_err()
            || !exports.insert(ExcelNameKey::new(descriptor.export_name))
            || !excel_names.insert(ExcelNameKey::new(descriptor.excel_name))
            || descriptor.signature.arguments.len() != descriptor.arguments.len()
            || descriptor.signature.validate().is_err()
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::REGISTRY,
            });
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreparedExcelString(Cow<'static, str>);

impl PreparedExcelString {
    fn new(value: String) -> Self {
        Self(Cow::Owned(value))
    }

    fn borrowed(value: &'static str) -> Self {
        Self(Cow::Borrowed(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreparedRegistration {
    pub descriptor_index: usize,
    pub export_name: &'static str,
    pub excel_name: &'static str,
    pub signature: RegistrationSignature,
    pub visibility: super::schema::FunctionVisibility,
    pub export_name_text: PreparedExcelString,
    pub excel_name_text: PreparedExcelString,
    pub category_text: PreparedExcelString,
    pub help_topic_text: PreparedExcelString,
    pub description_text: PreparedExcelString,
    pub argument_names: PreparedExcelString,
    pub type_text: PreparedExcelString,
    pub argument_help: Vec<PreparedExcelString>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreparedRegistrationSet {
    prepared: Vec<PreparedRegistration>,
}

impl PreparedRegistrationSet {
    pub(crate) fn iter(&self) -> impl Iterator<Item = &PreparedRegistration> {
        self.prepared.iter()
    }

    pub(crate) fn as_slice(&self) -> &[PreparedRegistration] {
        &self.prepared
    }
}

pub(crate) fn preflight_registration(
    descriptors: &[RegistrationDescriptor],
) -> XllResult<PreparedRegistrationSet> {
    validate_descriptors(descriptors)?;

    let mut prepared = Vec::with_capacity(descriptors.len());
    for (index, descriptor) in descriptors.iter().enumerate() {
        validate_excel_string(descriptor.export_name)?;
        validate_excel_string(descriptor.excel_name)?;
        validate_excel_string(descriptor.category)?;
        validate_excel_string(descriptor.help_topic)?;
        validate_excel_string(descriptor.description)?;

        for arg in descriptor.arguments {
            validate_excel_string(arg.name)?;
            validate_excel_string(arg.description)?;
        }

        let argument_names = match descriptor.arguments {
            [] => PreparedExcelString::borrowed(""),
            [argument] => PreparedExcelString::borrowed(argument.name),
            arguments => {
                let capacity = arguments
                    .iter()
                    .map(|argument| argument.name.len())
                    .sum::<usize>()
                    + arguments.len()
                    - 1;
                let mut names = String::with_capacity(capacity);
                for (index, argument) in arguments.iter().enumerate() {
                    if index != 0 {
                        names.push(',');
                    }
                    names.push_str(argument.name);
                }
                PreparedExcelString::new(names)
            }
        };
        validate_excel_string(argument_names.as_str())?;

        let encoded_sig = descriptor.signature.encode()?;
        validate_excel_string(&encoded_sig)?;

        let help_count = descriptor
            .arguments
            .len()
            .min(MAX_REGISTER_ARGUMENT_HELP_ENTRIES);
        let mut argument_help = Vec::with_capacity(help_count + usize::from(help_count != 0));
        argument_help.extend(
            descriptor.arguments[..help_count]
                .iter()
                .map(|argument| PreparedExcelString::borrowed(argument.description)),
        );
        if !argument_help.is_empty() {
            argument_help.push(PreparedExcelString::borrowed(""));
        }

        prepared.push(PreparedRegistration {
            descriptor_index: index,
            export_name: descriptor.export_name,
            excel_name: descriptor.excel_name,
            signature: descriptor.signature,
            visibility: descriptor.visibility,
            export_name_text: PreparedExcelString::borrowed(descriptor.export_name),
            excel_name_text: PreparedExcelString::borrowed(descriptor.excel_name),
            category_text: PreparedExcelString::borrowed(descriptor.category),
            help_topic_text: PreparedExcelString::borrowed(descriptor.help_topic),
            description_text: PreparedExcelString::borrowed(descriptor.description),
            argument_names,
            type_text: PreparedExcelString::new(encoded_sig),
            argument_help,
        });
    }

    Ok(PreparedRegistrationSet { prepared })
}

fn validate_excel_string(s: &str) -> XllResult<()> {
    xlfn_common::validate_excel_string(s).map_err(|_| XllError::Internal {
        diagnostic_id: crate::diagnostics::id::DiagnosticId::STRING_LENGTH,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::id::DiagnosticId;
    use crate::registration::{ArgumentAbi, ArgumentDescriptor, ReturnAbi};
    use xlfn_common::{ExecutionKind, FunctionVisibility};

    fn descriptor() -> RegistrationDescriptor {
        RegistrationDescriptor {
            export_name: "test_export",
            excel_name: "TEST.EXPORT",
            signature: RegistrationSignature {
                execution: ExecutionKind::ThreadSafe,
                arguments: &[],
                return_abi: ReturnAbi::Value12,
                volatile: false,
            },
            category: "価格",
            description: "Function help",
            help_topic: "",
            visibility: FunctionVisibility::Public,
            arguments: &[],
        }
    }

    #[test]
    fn descriptor_duplicates_keep_ascii_case_insensitive_identity() {
        let first = descriptor();
        for second in [
            RegistrationDescriptor {
                export_name: "TEST_EXPORT",
                excel_name: "TEST.OTHER",
                ..first
            },
            RegistrationDescriptor {
                export_name: "other_export",
                excel_name: "test.export",
                ..first
            },
        ] {
            assert!(matches!(
                preflight_registration(&[first, second]),
                Err(XllError::Internal {
                    diagnostic_id: DiagnosticId::REGISTRY,
                })
            ));
        }
        // Export-name normalization only folds ASCII, including for symbols
        // that contain non-ASCII text; Unicode case folding would be a change.
        assert!(
            preflight_registration(&[
                RegistrationDescriptor {
                    export_name: "export_é",
                    ..first
                },
                RegistrationDescriptor {
                    export_name: "export_É",
                    excel_name: "TEST.OTHER",
                    ..first
                },
            ])
            .is_ok()
        );
    }

    #[test]
    fn descriptor_validation_still_precedes_metadata_preparation() {
        let first = RegistrationDescriptor {
            category: "invalid\0category",
            ..descriptor()
        };
        let second = RegistrationDescriptor {
            export_name: "other_export",
            excel_name: "TEST.OTHER",
            signature: RegistrationSignature {
                arguments: &[ArgumentAbi::Value12],
                ..first.signature
            },
            ..descriptor()
        };
        assert!(matches!(
            preflight_registration(&[first, second]),
            Err(XllError::Internal {
                diagnostic_id: DiagnosticId::REGISTRY,
            })
        ));
        assert!(matches!(
            preflight_registration(&[first]),
            Err(XllError::Internal {
                diagnostic_id: DiagnosticId::STRING_LENGTH,
            })
        ));
    }

    #[test]
    fn argument_limit_keeps_all_names_and_the_help_trailer() {
        const ARGUMENTS: [ArgumentDescriptor; 255] = [ArgumentDescriptor {
            name: "値",
            description: "説明",
        }; 255];
        let descriptor = RegistrationDescriptor {
            arguments: &ARGUMENTS,
            signature: RegistrationSignature {
                arguments: &[ArgumentAbi::Value12; 255],
                ..descriptor().signature
            },
            ..descriptor()
        };
        let prepared = preflight_registration(&[descriptor]).unwrap();
        let cloned = prepared.clone();
        drop(prepared);
        let entry = &cloned.as_slice()[0];
        assert_eq!(entry.argument_names.as_str(), ["値"; 255].join(","));
        assert_eq!(entry.type_text.as_str(), format!("{}$", "Q".repeat(256)));
        assert_eq!(entry.argument_help.len(), 245);
        assert!(
            entry.argument_help[..244]
                .iter()
                .all(|help| help.as_str() == "説明")
        );
        assert_eq!(entry.argument_help[244].as_str(), "");

        // Help omitted from Excel's argument count is still validated before
        // host mutation; truncating the prepared list must not hide bad input.
        let mut invalid = ARGUMENTS;
        invalid[254].description = "invalid\0help";
        let invalid = RegistrationDescriptor {
            arguments: Box::leak(Box::new(invalid)),
            ..descriptor
        };
        assert!(matches!(
            preflight_registration(&[invalid]),
            Err(XllError::Internal {
                diagnostic_id: DiagnosticId::STRING_LENGTH,
            })
        ));
    }
}
