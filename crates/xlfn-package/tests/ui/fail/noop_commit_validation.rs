use std::path::Path;
use xlfn_package::{PreparedDirectoryCommit, SystemDistributionFileOps};

fn bypass(prepared: &PreparedDirectoryCommit, destination: &Path) {
    let _ = xlfn_package::distribution::commit_prepared_directory_with(
        prepared,
        destination,
        |_| Ok(()),
        |_| Ok(()),
        &SystemDistributionFileOps,
    );
}

fn main() {}
