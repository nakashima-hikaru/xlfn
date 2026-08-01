#![no_std]

// Compiled with --emit=obj by maintainers to verify the internal COFF name
// consumed by generated i686 .def aliases. This is intentionally not linked.
#[unsafe(no_mangle)]
pub extern "system" fn xll_export_probe(argument: *mut u8) -> *mut u8 {
    argument
}
