#!/usr/bin/env python3
"""Reject resurrection, unchecked overflow, and wrong final-pin classification."""

from pathlib import Path

from check_drain_gate_refinement import check_mutations


if __name__ == "__main__":
    check_mutations(
        Path("verification/verus/cache_lease"),
        Path("crates/xlfn/src/cache/pin_transitions.rs"),
        (Path("crates/xlfn/src/retirement_queue.rs"), Path("verification/verus/published_owner/src/heap_permission.rs"), *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")), Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs")), Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"),),
        {
            "zero-pin resurrection": ("if $pins == 0", "if false"),
            "pin overflow wraparound": ("else if $pins == <$word>::MAX", "else if false"),
            "wrong last-pin classification": ("else if $previous == 1", "else if $previous == 2"),
        },
    )
