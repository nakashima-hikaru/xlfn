#!/usr/bin/env python3
"""Sensitivity of typed PublishedOwner permission proofs (not production mutations)."""
from pathlib import Path
from verus_mutations import check_mutations

if __name__ == "__main__":
    check_mutations(
        Path("verification/verus/published_owner"),
        Path("verification/verus/published_owner/src/permission.rs"),
        (),
        {
            "adoption accepts a different allocation pointer": (
                "requires permission.inv(), permission.ptr() == pointer, permission.is_init(),",
                "requires permission.inv(), permission.is_init(),",
            ),
            "borrow omits the initialized matching permission invariant": (
                "pub fn borrow(&self) -> (value: &T)\n        requires self.inv(),",
                "pub fn borrow(&self) -> (value: &T)\n        requires true,",
            ),
        },
    )

    check_mutations(
        Path("verification/verus/published_owner"),
        Path("verification/verus/published_owner/src/heap_permission.rs"),
        (),
        {
            "allocator layout is not matched": (
                "&& a.size() == size_of::<T>() && a.align() == align_of::<T>()",
                "&& true",
            ),
            "allocator provenance is not matched": (
                "&& a.provenance() == memory.ptr()@.provenance",
                "&& true",
            ),
            "nonzero allocation lacks deallocation permission": (
                "None => size_of::<T>() == 0,",
                "None => true,",
            ),
        },
    )
