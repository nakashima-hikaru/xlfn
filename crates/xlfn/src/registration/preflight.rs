//! Pure registration validation and host-independent preparation.

use super::schema::MAX_REGISTER_ARGUMENT_HELP_ENTRIES;
use super::{ExcelNameKey, RegistrationDescriptor, RegistrationSignature};
use crate::{XllError, XllResult};
use std::collections::HashSet;

pub(crate) fn validate_descriptors(descriptors: &[RegistrationDescriptor]) -> XllResult<()> {
    let mut exports = HashSet::with_capacity(descriptors.len());
    let mut excel_names = HashSet::with_capacity(descriptors.len());
    for descriptor in descriptors {
        let max_arguments =
            xlfn_common::max_excel_function_arguments(descriptor.signature.execution);
        let argument_names = descriptor
            .arguments
            .iter()
            .map(|argument| argument.name)
            .collect::<Vec<_>>();
        if descriptor.export_name.is_empty()
            || descriptor.excel_name.is_empty()
            || descriptor.arguments.len() > max_arguments
            || xlfn_common::validate_argument_names(&argument_names).is_err()
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
pub(crate) struct PreparedExcelString(String);

impl PreparedExcelString {
    fn new(value: String) -> Self {
        Self(value)
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

        let argument_names = descriptor
            .arguments
            .iter()
            .map(|argument| argument.name)
            .collect::<Vec<_>>()
            .join(",");
        validate_excel_string(&argument_names)?;

        let encoded_sig = descriptor.signature.encode()?;
        validate_excel_string(&encoded_sig)?;

        let argument_help = descriptor
            .arguments
            .iter()
            .take(MAX_REGISTER_ARGUMENT_HELP_ENTRIES)
            .map(|argument| Ok(PreparedExcelString::new(argument.description.to_owned())))
            .collect::<XllResult<Vec<_>>>()?;
        let mut argument_help = argument_help;
        if !argument_help.is_empty() {
            argument_help.push(PreparedExcelString::new(String::new()));
        }

        prepared.push(PreparedRegistration {
            descriptor_index: index,
            export_name: descriptor.export_name,
            excel_name: descriptor.excel_name,
            signature: descriptor.signature,
            visibility: descriptor.visibility,
            export_name_text: PreparedExcelString::new(descriptor.export_name.to_owned()),
            excel_name_text: PreparedExcelString::new(descriptor.excel_name.to_owned()),
            category_text: PreparedExcelString::new(descriptor.category.to_owned()),
            help_topic_text: PreparedExcelString::new(descriptor.help_topic.to_owned()),
            description_text: PreparedExcelString::new(descriptor.description.to_owned()),
            argument_names: PreparedExcelString::new(argument_names),
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
