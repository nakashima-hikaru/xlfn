use super::*;

pub type PackageResult<T = ()> = Result<T, PackageError>;
pub const SYSTEM_IMPORT_POLICY_VERSION: &str = "windows-system-v1";
pub const REQUIRED_XLL_EXPORTS: &[&str] = &[
    "xlAutoOpen",
    "xlAutoClose",
    "xlAutoRemove",
    "xlAutoFree12",
    "xlAddInManagerInfo12",
];

pub(crate) const MAX_PREPARED_ENTRIES: usize = 100_000;
pub(crate) const MAX_PREPARED_BYTES: u64 = 512 * 1024 * 1024;

pub(crate) const CRT_MARKER_MAGIC: &[u8; 8] = b"XLFNCRT\0";
pub(crate) const CRT_MARKER_SCHEMA: u8 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectiveCrtPolicy {
    Dynamic,
    Static,
}

impl EffectiveCrtPolicy {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Dynamic => "dynamic",
            Self::Static => "static",
        }
    }
}

pub(crate) fn parse_crt_marker(data: &[u8]) -> PackageResult<EffectiveCrtPolicy> {
    if data.len() < 16 || &data[..CRT_MARKER_MAGIC.len()] != CRT_MARKER_MAGIC {
        return Err("malformed .xlfncrt marker: marker must start at section offset 0".into());
    }
    if data
        .windows(CRT_MARKER_MAGIC.len())
        .filter(|candidate| *candidate == CRT_MARKER_MAGIC)
        .count()
        != 1
    {
        return Err("malformed .xlfncrt marker: multiple magic values".into());
    }
    if data[16..].iter().any(|byte| *byte != 0) {
        return Err("malformed .xlfncrt marker: non-zero section padding".into());
    }
    let marker = &data[..16];
    if marker[8] != CRT_MARKER_SCHEMA {
        return Err(format!("unsupported .xlfncrt marker schema {}", marker[8]).into());
    }
    if marker[10..].iter().any(|byte| *byte != 0) {
        return Err("malformed .xlfncrt marker: reserved bytes are not zero".into());
    }
    match marker[9] {
        0 => Ok(EffectiveCrtPolicy::Dynamic),
        1 => Ok(EffectiveCrtPolicy::Static),
        value => Err(format!("invalid .xlfncrt policy value {value}").into()),
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ImportTarget {
    Name(String),
    Ordinal(u16),
}

impl ImportTarget {
    pub(crate) fn display(&self) -> String {
        match self {
            Self::Name(name) => name.clone(),
            Self::Ordinal(ordinal) => format!("#{ordinal}"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Pe(#[from] object::Error),
    #[error(transparent)]
    Utf8(#[from] std::str::Utf8Error),
    #[error("{target}: bundle source is busy or open for modification: {path}: {source}")]
    BundleSourceBusy {
        target: String,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{target}: bundle source changed while snapshotting: {path}")]
    UnstableBundleSource { target: String, path: PathBuf },
    #[error("{target}: staged artifact changed after verification: {path}")]
    StagedArtifactChanged { target: String, path: PathBuf },
    #[error("private staging directory path was replaced: {path}")]
    StagingDirectoryReplaced { path: PathBuf },
    #[error("invalid build manifest: {0}")]
    InvalidBuildManifest(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Message(String),
}

impl From<String> for PackageError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<&str> for PackageError {
    fn from(message: &str) -> Self {
        Self::Message(message.to_owned())
    }
}
