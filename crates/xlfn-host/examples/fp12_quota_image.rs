//! Native process qualification image. This is not an Excel add-in.

#[cfg(windows)]
use xlfn_host::process_quota::{PublishedCharge, Reservation, read_metrics};

#[cfg(windows)]
#[repr(C)]
struct RetainedAllocation {
    charge: PublishedCharge,
    payload: [u8; 64],
}

/// Publishes a small allocation whose lifetime deliberately outlives this DLL.
#[cfg(windows)]
#[unsafe(no_mangle)]
pub extern "C" fn fp12_fixture_publish() -> usize {
    let bytes = size_of::<RetainedAllocation>();
    let Ok(reserved) = Reservation::reserve(bytes) else {
        return 0;
    };
    let charge = reserved.publish();
    // The process releases the plain allocation at exit; never run a DLL
    // destructor or put an image-owned pointer in the shared quota header.
    let _ = Box::leak(Box::new(RetainedAllocation {
        charge,
        payload: [0; 64],
    }));
    bytes
}

/// Returns live published bytes, or usize::MAX if attachment fails.
#[cfg(windows)]
#[unsafe(no_mangle)]
pub extern "C" fn fp12_fixture_published_bytes() -> usize {
    read_metrics().map_or(usize::MAX, |metrics| metrics.published_bytes)
}

/// Distinguishes admission, cap rejection, and unrelated quota failures.
#[cfg(windows)]
#[unsafe(no_mangle)]
pub extern "C" fn fp12_fixture_probe(bytes: usize) -> i32 {
    use xlfn_engine::error::{InputError, XllError};
    match Reservation::reserve(bytes) {
        Ok(reserved) => {
            drop(reserved);
            1
        }
        Err(XllError::Input {
            reason: InputError::TooLarge { .. },
            ..
        }) => 0,
        Err(_) => -1,
    }
}
