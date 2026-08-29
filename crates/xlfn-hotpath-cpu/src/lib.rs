#![no_std]

// This crate exists only to enable hotpath's CPU backend on Unix targets.
// Keeping that feature behind a target-specific dependency prevents a Windows
// all-features build from selecting hotpath's unsupported backend.
