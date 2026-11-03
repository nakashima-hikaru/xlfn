use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Architecture {
    X86,
    X64,
}

impl Architecture {
    pub fn parse(target: &str) -> PackageResult<Self> {
        match target {
            "i686-pc-windows-msvc" => Ok(Self::X86),
            "x86_64-pc-windows-msvc" => Ok(Self::X64),
            _ => Err(format!("unsupported Windows target {target:?}").into()),
        }
    }
    pub const fn machine(self) -> u16 {
        match self {
            Self::X86 => 0x014c,
            Self::X64 => 0x8664,
        }
    }

    pub(crate) fn pe_machine(self) -> object::pe::Machine {
        match self {
            Self::X86 => IMAGE_FILE_MACHINE_I386,
            Self::X64 => IMAGE_FILE_MACHINE_AMD64,
        }
    }
}
