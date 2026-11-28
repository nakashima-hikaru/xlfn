//! Registration validation before the first Excel callback.

use super::RegistrationDescriptor;
use super::schema::{ExecutionKind, MAX_REGISTER_ARGUMENT_HELP_ENTRIES, ValidatedSignature};
use crate::{XllError, XllResult};
use rustc_hash::FxHashSet;
use std::hash::{Hash, Hasher};

/// Preflight only borrows descriptor names. Match the retained ledger's
/// ASCII-only host identity without building a canonical heap string.
#[derive(Clone, Copy)]
struct BorrowedExcelName(&'static str);

impl PartialEq for BorrowedExcelName {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(other.0)
    }
}

impl Eq for BorrowedExcelName {}

impl Hash for BorrowedExcelName {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_usize(self.0.len());
        let mut normalized = [0; 64];
        for bytes in self.0.as_bytes().chunks(normalized.len()) {
            let chunk = &mut normalized[..bytes.len()];
            chunk.copy_from_slice(bytes);
            chunk.make_ascii_uppercase();
            state.write(chunk);
        }
    }
}

/// Checked metadata and the layout of its generated Excel strings.
/// A registration call materializes or borrows the counted text it needs.
#[derive(Clone)]
pub(crate) struct ValidatedRegistration {
    descriptor: RegistrationDescriptor,
    signature: ValidatedSignature,
    signature_units: usize,
    argument_name_units: usize,
    argument_help_count: usize,
}

impl ValidatedRegistration {
    pub(super) fn descriptor(&self) -> &RegistrationDescriptor {
        &self.descriptor
    }

    pub(crate) fn execution(&self) -> ExecutionKind {
        self.signature.execution()
    }

    pub(super) fn signature_text(&self) -> (usize, impl Iterator<Item = u16> + use<>) {
        (
            self.signature_units,
            self.signature.codes().flat_map(str::encode_utf16),
        )
    }

    pub(super) fn argument_names(&self) -> (usize, impl Iterator<Item = u16> + use<>) {
        let units = self
            .descriptor
            .arguments
            .iter()
            .enumerate()
            .flat_map(|(index, argument)| {
                (index != 0)
                    .then_some(u16::from(b','))
                    .into_iter()
                    .chain(argument.name.encode_utf16())
            });
        (self.argument_name_units, units)
    }

    pub(super) fn argument_help(&self) -> &[super::ArgumentDescriptor] {
        &self.descriptor.arguments[..self.argument_help_count]
    }
}

#[derive(Clone)]
pub(crate) struct ValidatedRegistrationSet {
    entries: Vec<ValidatedRegistration>,
}

impl ValidatedRegistrationSet {
    pub(crate) fn iter(&self) -> impl Iterator<Item = &ValidatedRegistration> {
        self.entries.iter()
    }

    pub(crate) fn as_slice(&self) -> &[ValidatedRegistration] {
        &self.entries
    }
}

pub(crate) fn preflight_registration(
    descriptors: &[RegistrationDescriptor],
) -> XllResult<ValidatedRegistrationSet> {
    let mut entries = Vec::with_capacity(descriptors.len());
    // Preserve the priority of structural errors across the entire descriptor
    // set before checking its string metadata.
    {
        let mut exports =
            FxHashSet::with_capacity_and_hasher(descriptors.len(), Default::default());
        let mut excel_names =
            FxHashSet::with_capacity_and_hasher(descriptors.len(), Default::default());
        for &descriptor in descriptors {
            let max_arguments =
                xlfn_common::max_excel_function_arguments(descriptor.signature.execution);
            if descriptor.export_name.is_empty()
                || xlfn_common::validate_function_name(descriptor.excel_name).is_err()
                || descriptor.arguments.len() > max_arguments
            {
                return Err(registry_error());
            }
            let argument_name_units = xlfn_common::argument_names_utf16_len(
                descriptor.arguments.iter().map(|argument| argument.name),
            )
            .map_err(|_| registry_error())?;
            if !exports.insert(BorrowedExcelName(descriptor.export_name))
                || !excel_names.insert(BorrowedExcelName(descriptor.excel_name))
                || descriptor.signature.arguments.len() != descriptor.arguments.len()
            {
                return Err(registry_error());
            }
            let signature =
                ValidatedSignature::try_from(descriptor.signature).map_err(|_| registry_error())?;
            // Registration codes and modifiers are ASCII and each code is at
            // most two UTF-16 units. The checked argument count keeps the
            // complete signature well below the counted string limit.
            let signature_units = signature.codes().map(str::len).sum();
            entries.push(ValidatedRegistration {
                descriptor,
                signature,
                signature_units,
                argument_name_units,
                argument_help_count: descriptor
                    .arguments
                    .len()
                    .min(MAX_REGISTER_ARGUMENT_HELP_ENTRIES),
            });
        }
    }

    for entry in &entries {
        let descriptor = &entry.descriptor;
        for value in [
            descriptor.export_name,
            descriptor.excel_name,
            descriptor.category,
            descriptor.help_topic,
            descriptor.description,
        ] {
            validate_excel_string(value)?;
        }
        if descriptor.category.encode_utf16().count() > 255 {
            return Err(crate::error::construct::internal(
                crate::diagnostics::catalog::STRING_LENGTH,
            ));
        }
        // Also validate descriptions outside Excel's argument help limit.
        for arg in descriptor.arguments {
            validate_excel_string(arg.description)?;
        }
    }

    Ok(ValidatedRegistrationSet { entries })
}

const fn registry_error() -> XllError {
    crate::error::construct::internal(crate::diagnostics::catalog::REGISTRY)
}

fn validate_excel_string(s: &str) -> XllResult<()> {
    xlfn_common::validate_excel_string(s)
        .map_err(|_| crate::error::construct::internal(crate::diagnostics::catalog::STRING_LENGTH))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registration::{ArgumentAbi, ArgumentDescriptor, RegistrationSignature, ReturnAbi};
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
                (preflight_registration(&[first, second]))
                    .as_ref()
                    .map_err(crate::error::construct::view),
                Err(crate::error::construct::ErrorView::Internal {
                    diagnostic_id: crate::diagnostics::catalog::REGISTRY,
                    ..
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
    fn borrowed_duplicate_names_match_retained_identity_across_chunks() {
        use crate::registration::ExcelNameKey;
        use std::hash::{BuildHasher, RandomState};

        let samples = [
            "",
            "alPHA",
            "ALpha",
            "Älpha",
            "älpha",
            "αaあ",
            "αAあ",
            "ThisFixtureNameCrossesTheCanonicalizationScratchChunkBoundary0000éSecondChunk",
            "THISFIXTURENAMECROSSESTHECANONICALIZATIONSCRATCHCHUNKBOUNDARY0000éSECONDCHUNK",
        ];
        let builder = RandomState::new();
        for left in samples {
            for right in samples {
                let a = BorrowedExcelName(left);
                let b = BorrowedExcelName(right);
                assert_eq!(a == b, ExcelNameKey::new(left) == ExcelNameKey::new(right));
                if a == b {
                    assert_eq!(builder.hash_one(a), builder.hash_one(b));
                }
            }
        }
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
            (preflight_registration(&[first, second]))
                .as_ref()
                .map_err(crate::error::construct::view),
            Err(crate::error::construct::ErrorView::Internal {
                diagnostic_id: crate::diagnostics::catalog::REGISTRY,
                ..
            })
        ));
        assert!(matches!(
            (preflight_registration(&[first]))
                .as_ref()
                .map_err(crate::error::construct::view),
            Err(crate::error::construct::ErrorView::Internal {
                diagnostic_id: crate::diagnostics::catalog::STRING_LENGTH,
                ..
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
        let (names_length, names) = entry.argument_names();
        let names: Vec<_> = names.collect();
        assert_eq!(names_length, names.len());
        assert_eq!(String::from_utf16(&names).unwrap(), ["値"; 255].join(","));
        let (signature_length, signature) = entry.signature_text();
        let signature: Vec<_> = signature.collect();
        assert_eq!(signature_length, signature.len());
        assert_eq!(
            String::from_utf16(&signature).unwrap(),
            format!("{}$", "Q".repeat(256))
        );
        assert_eq!(entry.argument_help().len(), 244);
        assert!(
            entry
                .argument_help()
                .iter()
                .all(|argument| argument.description == "説明")
        );

        // Help omitted from Excel's argument count is still validated before
        // host mutation; truncating the prepared list must not hide bad input.
        const INVALID_ARGUMENTS: [ArgumentDescriptor; 255] = {
            let mut invalid = ARGUMENTS;
            invalid[254].description = "invalid\0help";
            invalid
        };
        let invalid = RegistrationDescriptor {
            arguments: &INVALID_ARGUMENTS,
            ..descriptor
        };
        assert!(matches!(
            (preflight_registration(&[invalid]))
                .as_ref()
                .map_err(crate::error::construct::view),
            Err(crate::error::construct::ErrorView::Internal {
                diagnostic_id: crate::diagnostics::catalog::STRING_LENGTH,
                ..
            })
        ));

        const LONG_CATEGORY_BYTES: [u8; 256] = [b'c'; 256];
        let long_category = std::str::from_utf8(&LONG_CATEGORY_BYTES).unwrap();
        let over_category = RegistrationDescriptor {
            category: long_category,
            ..self::descriptor()
        };
        assert!(matches!(
            (preflight_registration(&[over_category]))
                .as_ref()
                .map_err(crate::error::construct::view),
            Err(crate::error::construct::ErrorView::Internal {
                diagnostic_id: crate::diagnostics::catalog::STRING_LENGTH,
                ..
            })
        ));
    }
}
