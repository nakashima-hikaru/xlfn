//! Shared validation and staging for public and repository XLL packaging.
//!
//! The package pipeline is deliberately closed-world: bundle sources are
//! snapshotted, PE imports and exports are validated, staged artifacts are
//! hashed, and commit-time identity checks protect the final rename. The
//! Windows-specific private-directory checks enforce the same trust boundary
//! used by the transactional distributor.

#![deny(unsafe_code)]

use fs_err as fs;
use object::FileKind;
use object::endian::LittleEndian as LE;
use object::pe::{
    IMAGE_FILE_DLL, IMAGE_FILE_EXECUTABLE_IMAGE, IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_I386,
    IMAGE_FILE_SYSTEM, IMAGE_SCN_MEM_EXECUTE,
};
use object::read::pe::{
    ExportAddressIndex, ExportOrdinal, ExportTarget, ImageNtHeaders, Import as PeImport, PeFile,
    PeFile32, PeFile64,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[cfg(target_os = "windows")]
#[allow(
    unsafe_code,
    clippy::undocumented_unsafe_blocks,
    reason = "Windows Win32 FFI bindings"
)]
mod win32;

mod architecture;
mod artifact;
mod bundle;
mod commit;
#[doc(hidden)]
pub mod distribution;
mod error;
mod fs_identity;
mod manifest;
mod names;
mod pe;
mod staging;

pub(crate) use bundle::*;
pub(crate) use commit::*;
pub(crate) use error::*;
pub(crate) use fs_identity::*;
pub(crate) use manifest::*;
pub(crate) use names::*;
pub(crate) use pe::*;
pub(crate) use staging::*;

pub use architecture::Architecture;
pub use artifact::{VerifiedArtifact, VerifiedPackage, verify_staged_package};
pub use bundle::{
    BundleMetadata, ResolvedBundle, StagedBundle, resolve_bundle_files,
    resolve_bundle_files_with_metadata, resolve_bundle_files_with_policy, snapshot_file,
    stage_bundle, verify_bundle_files,
};
pub use commit::{CommitSourceLease, PreparedDirectoryCommit, PreparedPackageCommit};
pub use distribution::{
    CleanupOutcome, CommitOutcome, DistributionError, DistributionFileOps,
    DistributionRecoveryError, DistributionRecoveryQuarantineError, DistributionResult,
    PreparedDistribution,
};
pub use error::{
    EffectiveCrtPolicy, ImportTarget, PackageError, PackageResult, REQUIRED_XLL_EXPORTS,
    SYSTEM_IMPORT_POLICY_VERSION,
};
pub use fs_identity::{DirectoryIdentity, directory_identity};
pub use manifest::{
    BUILD_MANIFEST_SCHEMA, BuildManifest, BuildManifestInput, BundlePolicy, BundleSource,
    CargoConstraints, CrtManifest, FeatureSelection, IntegrityMetadata,
};
pub use names::{validate_directory_path, validate_path_components, validate_windows_basename};
pub use pe::{
    ExportSymbol, ForwardedExport, PeInfo, inspect_pe, parse_pe_bytes, sha256,
    verify_pe_dependency_closure, verify_xll,
};
pub use staging::PrivateStagingDirectory;

#[cfg(test)]
mod tests;
