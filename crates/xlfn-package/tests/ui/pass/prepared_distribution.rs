use std::{io, path::Path};
use xlfn_package::{
    DistributionFileOps, DistributionResult, PreparedDirectoryCommit, PreparedDistribution,
    PreparedPackageCommit, SystemDistributionFileOps,
};

struct FileOps;

impl DistributionFileOps for FileOps {
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        SystemDistributionFileOps.rename(from, to)
    }
    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        SystemDistributionFileOps.remove_dir_all(path)
    }
    fn sync_directory(&self, path: &Path) -> io::Result<()> {
        SystemDistributionFileOps.sync_directory(path)
    }
}

fn commit(
    root: PreparedDirectoryCommit,
    package: PreparedPackageCommit,
    destination: &Path,
) -> DistributionResult {
    PreparedDistribution::new(root)
        .with_package(package)?
        .commit_with(destination, &FileOps)?;
    Ok(())
}

fn main() {
    let _ = commit;
}
