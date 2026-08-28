//! Host-boundary reporting and fail-stop behavior.
//!
//! Lifecycle state transitions stay free of filesystem, tracing, and Win32
//! reporting policy. This module is the narrow boundary adapter used when a
//! runtime operation has to communicate a failure to the host.

pub(crate) mod host;

use crate::diagnostics::AddinId;
use crate::error::XllError;
use std::panic::{AssertUnwindSafe, catch_unwind};

pub(crate) fn write_startup_log(addin_id: &AddinId, message: &str) {
    #[cfg(target_os = "windows")]
    {
        use std::fs;
        let Some(local) = std::env::var_os("LOCALAPPDATA") else {
            return;
        };
        let directory = std::path::PathBuf::from(local)
            .join(addin_id.as_str())
            .join("logs");
        if fs::create_dir_all(&directory).is_err() {
            return;
        }
        let _ = crate::diagnostics::append_startup_log(&directory.join("startup.log"), message);
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (addin_id, message);
}

pub(crate) fn report_cleanup_issue(issue: &crate::shutdown::CleanupIssue) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        tracing::warn!(
            component = issue.component,
            kind = ?issue.kind,
            error = %issue.error,
            "cleanup issue during shutdown"
        );
    }));
    report_boundary_error(issue.component, &issue.error);
}

#[allow(
    unsafe_code,
    reason = "Windows diagnostic output is the host-boundary FFI leaf"
)]
pub(crate) fn report_boundary_error(boundary: &'static str, error: &XllError) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        crate::diagnostics::report_no_unwind(boundary, error);
        let message = format!("xlfn {boundary}: {error}\n");
        #[cfg(target_os = "windows")]
        {
            use crate::win32::OutputDebugStringW;

            let mut wide = message.encode_utf16().collect::<Vec<_>>();
            wide.push(0);
            // SAFETY: wide is nul-terminated and live for this synchronous call.
            unsafe { OutputDebugStringW(wide.as_ptr()) };
        }
        #[cfg(not(target_os = "windows"))]
        {
            eprint!("{message}");
        }
    }));
}

#[cold]
pub(crate) fn fail_stop_invariant(boundary: &'static str, error: &XllError) -> ! {
    report_boundary_error(boundary, error);

    #[cfg(not(test))]
    std::process::abort();

    #[cfg(test)]
    panic!("internal unload invariant failed at {boundary}: {error}");
}

#[cold]
pub(crate) fn fail_stop_module_residency(error: &XllError) -> ! {
    report_boundary_error("xlAutoOpen module residency", error);

    #[cfg(not(test))]
    std::process::abort();

    #[cfg(test)]
    panic!("module residency acquisition failed: {error}");
}
