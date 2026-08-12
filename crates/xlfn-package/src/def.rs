use std::fmt::Write as _;

/// Generator for MSVC module definition (`.def`) files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleDefinition;

impl ModuleDefinition {
    /// Generates MSVC module definition (`.def`) file contents.
    /// `DllCanUnloadNow` and `DllGetClassObject` are marked `PRIVATE` as required
    /// by MSVC linker guidelines to avoid LNK4104 warnings while keeping the COM
    /// entry points exported in the DLL export table.
    pub fn generate<S: AsRef<str>>(exports: impl IntoIterator<Item = S>) -> String {
        let mut def = String::from("EXPORTS\n");
        for export in exports {
            let name = export.as_ref();
            if name == "DllCanUnloadNow" || name == "DllGetClassObject" {
                let _ = writeln!(def, "    {name} PRIVATE");
            } else {
                let _ = writeln!(def, "    {name}");
            }
        }
        def
    }

    /// Generates standard XLL module definition file contents containing
    /// required XLL framework exports with COM entry points marked `PRIVATE`.
    pub fn default_xll() -> String {
        Self::generate(crate::REQUIRED_XLL_EXPORTS)
    }
}
