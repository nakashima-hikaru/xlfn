#!/usr/bin/env python3
"""Reject unsafe shared lookup branches and incorrect pin arithmetic."""

from pathlib import Path

from check_drain_gate_refinement import check_mutations


if __name__ == "__main__":
    check_mutations(
        Path("verification/verus/cache_lease"),
        Path("crates/xlfn/src/cache/pin_transitions.rs"),
        (Path("crates/xlfn/src/cache/node_layout.rs"), Path("crates/xlfn/src/retirement_queue.rs"), Path("verification/verus/published_owner/src/heap_permission.rs"), *tuple(Path("verification/verus/drain_gate/src").glob("*.rs")), Path("crates/xlfn-kernel/src/drain_gate/protocol.rs"), Path("crates/xlfn-kernel/src/rotating_read_domain/protocol.rs"),
         *tuple(Path("verification/verus/rotating_read_domain/src").glob("*.rs")), Path("crates/xlfn-kernel/src/sealable_counter/transitions.rs"),),
        {
            "lookup accepts an ineligible observation": ("if !$eligible {", "if false {"),
            "lookup returns a lease after failed resident recheck": ("if !$resident {", "if false {"),
            "lookup rolls back a still-resident node": ("if !$resident {", "if true {"),
            "zero-pin resurrection": ("if $pins == 0", "if false"),
            "pin overflow wraparound": ("else if $pins == <$word>::MAX", "else if false"),
            "failed CAS grants a pin": ("Err(current) => { $raw = current; }", "Err(current) => { return $success; }"),
            "failed CAS fabricates zero": ("Err(current) => { $raw = current; }", "Err(current) => { $raw = 0; }"),
            "nonfinal release grants retirement": ("Release::StillPinned => $pinned,", "Release::StillPinned => $last,"),
            "final release loses retirement": ("                    $last\n", "                    $pinned\n"),
            "wrong last-pin classification": ("else if $previous == 1", "else if $previous == 2"),
        },
    )
