#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EmbeddedCrtPolicy {
    #[cfg(not(target_feature = "crt-static"))]
    Dynamic = 0,
    #[cfg(target_feature = "crt-static")]
    Static = 1,
}

#[repr(C)]
struct CrtPolicyMarker {
    magic: [u8; 8],
    schema: u8,
    policy: EmbeddedCrtPolicy,
    reserved: [u8; 6],
}

#[cfg(target_feature = "crt-static")]
const EFFECTIVE_POLICY: EmbeddedCrtPolicy = EmbeddedCrtPolicy::Static;

#[cfg(not(target_feature = "crt-static"))]
const EFFECTIVE_POLICY: EmbeddedCrtPolicy = EmbeddedCrtPolicy::Dynamic;

#[used]
#[cfg_attr(target_env = "msvc", unsafe(link_section = ".xlfncrt"))]
static CRT_POLICY_MARKER: CrtPolicyMarker = CrtPolicyMarker {
    magic: *b"XLFNCRT\0",
    schema: 1,
    policy: EFFECTIVE_POLICY,
    reserved: [0; 6],
};

#[inline(never)]
pub(crate) fn effective_crt_policy() -> EmbeddedCrtPolicy {
    // A volatile read keeps a relocation to the marker in a mandatory XLL
    // lifecycle path, preventing PE link-time dead stripping of the section.
    // SAFETY: the address refers to an immutable, correctly aligned static and
    // is read without mutation for the lifetime of the process.
    unsafe { (&raw const CRT_POLICY_MARKER.policy).read_volatile() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_has_the_stable_binary_layout() {
        assert_eq!(std::mem::size_of::<CrtPolicyMarker>(), 16);
        assert_eq!(CRT_POLICY_MARKER.magic, *b"XLFNCRT\0");
        assert_eq!(CRT_POLICY_MARKER.schema, 1);
    }
}
