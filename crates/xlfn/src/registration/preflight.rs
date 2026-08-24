//! Pure registration validation and host-independent preparation.

use super::{
    ArgumentDescriptor, ExcelNameKey, MAX_EXCEL_FUNCTION_ARGUMENTS, RegistrationDescriptor,
    RegistrationSignature, ResultAbi,
};
use crate::{XllError, XllResult};
use std::collections::HashSet;

pub(crate) fn validate_descriptors(descriptors: &[RegistrationDescriptor]) -> XllResult<()> {
    let mut exports = HashSet::with_capacity(descriptors.len());
    let mut excel_names = HashSet::with_capacity(descriptors.len());
    for descriptor in descriptors {
        let max_arguments = if descriptor.signature.result == ResultAbi::AsyncVoid {
            MAX_EXCEL_FUNCTION_ARGUMENTS - 1
        } else {
            MAX_EXCEL_FUNCTION_ARGUMENTS
        };
        if descriptor.export_name.is_empty()
            || descriptor.excel_name.is_empty()
            || descriptor.arguments.len() > max_arguments
            || !valid_argument_names(descriptor.arguments)
            || !exports.insert(descriptor.export_name.to_ascii_lowercase())
            || !excel_names.insert(ExcelNameKey::new(descriptor.excel_name))
            || descriptor.signature.arguments.len() != descriptor.arguments.len()
            || descriptor.signature.encode().is_err()
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::REGISTRY,
            });
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreparedRegistration {
    pub descriptor_index: usize,
    pub export_name: &'static str,
    pub excel_name: &'static str,
    pub category: &'static str,
    pub help_topic: &'static str,
    pub description: &'static str,
    pub arguments: &'static [ArgumentDescriptor],
    pub signature: RegistrationSignature,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreparedRegistrationSet {
    pub(crate) prepared: Vec<PreparedRegistration>,
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

        let argument_names = descriptor
            .arguments
            .iter()
            .map(|argument| argument.name)
            .collect::<Vec<_>>()
            .join(",");
        validate_excel_string(&argument_names)?;

        let encoded_sig = descriptor.signature.encode()?;
        validate_excel_string(&encoded_sig)?;

        prepared.push(PreparedRegistration {
            descriptor_index: index,
            export_name: descriptor.export_name,
            excel_name: descriptor.excel_name,
            category: descriptor.category,
            help_topic: descriptor.help_topic,
            description: descriptor.description,
            arguments: descriptor.arguments,
            signature: descriptor.signature,
        });
    }

    Ok(PreparedRegistrationSet { prepared })
}

fn validate_excel_string(s: &str) -> XllResult<()> {
    let utf16_len = s.encode_utf16().count();
    if utf16_len > crate::utf16::EXCEL_STRING_LIMIT || s.contains('\0') {
        Err(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::STRING_LENGTH,
        })
    } else {
        Ok(())
    }
}

fn valid_argument_names(arguments: &[ArgumentDescriptor]) -> bool {
    let mut joined_utf16_len = arguments.len().saturating_sub(1);
    for argument in arguments {
        let name = argument.name;
        let utf16_len = name.encode_utf16().count();
        if name.is_empty()
            || name.contains([',', '\0', '\r', '\n'])
            || utf16_len > crate::utf16::EXCEL_STRING_LIMIT
        {
            return false;
        }
        joined_utf16_len = joined_utf16_len.saturating_add(utf16_len);
    }
    joined_utf16_len <= crate::utf16::EXCEL_STRING_LIMIT
}
