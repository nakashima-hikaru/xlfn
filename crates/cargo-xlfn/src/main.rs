//! Cargo subcommand for checking, staging, and packaging Rust Excel XLLs.
//!
//! `cargo xlfn check` validates the selected Windows target and CRT policy,
//! while `cargo xlfn package` builds a closed-world package and commits it through
//! the transactional staging API in `xlfn-package`.

use anyhow::{Context, anyhow, bail};
use cargo_metadata::{CargoOpt, Metadata, MetadataCommand, Package};
use fs_err as fs;
use serde_json::json;
use std::collections::BTreeMap;
#[cfg(test)]
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use usage::{Args, Cli, Subcommands, ValueEnum};
use xlfn_package::{BundleMetadata, validate_windows_basename};
#[cfg(test)]
use xlfn_package::{DirectoryIdentity, directory_identity};

#[cfg(target_os = "windows")]
#[allow(
    clippy::undocumented_unsafe_blocks,
    reason = "FFI bindings in win32 module"
)]
mod win32;

mod crt;

use crt::{CrtObservation, CrtPolicy, ResolvedCrtPolicy};

type Result<T = ()> = anyhow::Result<T>;

fn main() {
    if crt::wrapper_mode_requested() {
        match crt::run_wrapper() {
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(error) => {
                eprintln!("cargo xlfn rustc wrapper: {error:#}");
                std::process::exit(1);
            }
        }
    }
    if let Err(error) = run() {
        eprintln!("cargo xlfn: {error}");
        std::process::exit(1);
    }
}

mod cargo;
mod check;
mod cli;
mod distribution;
mod metadata;
mod package;
mod target;

pub(crate) use cargo::*;
pub(crate) use check::*;
pub(crate) use cli::*;
pub(crate) use distribution::*;
pub(crate) use metadata::*;
pub(crate) use package::*;
pub(crate) use target::*;
#[cfg(test)]
mod tests;
