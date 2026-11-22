//! Native host adapter for xlfn. No dependency on the facade or its add-ins.

/// Affine startup, dispatch, and scoped drain ownership.
#[doc(hidden)]
pub mod drain;

/// Owned COM interface and native Automation transport primitives.
#[doc(hidden)]
#[cfg(windows)]
pub mod com_transport;

/// COM-aware waiting and owned Win32 event handles.
#[doc(hidden)]
#[cfg(windows)]
pub mod com_wait;

/// Process-owned FP12 accounting and platform mapping.
#[doc(hidden)]
pub mod process_quota;

/// Native Excel registration arguments and scratch preparation.
#[doc(hidden)]
pub mod registration;

/// Generated Windows ABI used by the platform adapter.
#[doc(hidden)]
#[cfg(windows)]
pub mod win32;

/// Failure to acquire or release the module reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModuleReferenceError;

/// An explicit loader reference associated with an image address.
///
/// It has no automatic release: the caller's unload protocol decides whether
/// a native release is safe and may deliberately retain this reference.
#[derive(Debug)]
#[must_use]
pub struct NativeModuleReference {
    #[cfg(windows)]
    module: Option<core::num::NonZeroUsize>,
}

impl NativeModuleReference {
    /// Acquires a reference to the image containing the address.
    ///
    /// Windows interprets a non-null address as an image location; the
    /// address is never read as memory by this adapter.
    pub fn acquire(anchor: *const ()) -> Result<Self, ModuleReferenceError> {
        #[cfg(windows)]
        {
            if anchor.is_null() {
                return Err(ModuleReferenceError);
            }
            let mut module = core::ptr::null_mut();
            // SAFETY: FROM_ADDRESS interprets the argument as an address,
            // and the output pointer refers to one writable stack slot.
            let acquired = unsafe {
                win32::GetModuleHandleExW(
                    win32::GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS as u32,
                    anchor.cast(),
                    &mut module,
                )
            } != 0;
            if !acquired {
                return Err(ModuleReferenceError);
            }
            let module =
                core::num::NonZeroUsize::new(module as usize).ok_or(ModuleReferenceError)?;
            Ok(Self {
                module: Some(module),
            })
        }
        #[cfg(not(windows))]
        {
            let _ = anchor;
            Ok(Self {})
        }
    }

    /// Observes whether the adapter still owns a Windows loader reference.
    #[cfg(windows)]
    #[must_use]
    pub fn is_held(&self) -> bool {
        self.module.is_some()
    }

    /// Releases this reference, retaining it if the native release fails.
    ///
    /// # Safety
    ///
    /// The caller must certify that releasing the reference cannot prematurely
    /// unload an image with live code or accessible objects, including the
    /// caller's execution and return path. This adapter cannot issue that proof.
    pub unsafe fn try_release(&mut self) -> Result<(), ModuleReferenceError> {
        #[cfg(windows)]
        if let Some(module) = self.module {
            // SAFETY: this is precisely the loader reference acquired by
            // GetModuleHandleExW; the caller supplies the unload certificate.
            if unsafe { win32::FreeLibrary(module.get() as _) } == 0 {
                return Err(ModuleReferenceError);
            }
            self.module = None;
        }
        Ok(())
    }
}

/// The callback ABI consumed by the facade's admission and result owner.
#[doc(hidden)]
pub use xlfn_sys::excel_free;

/// Distinguishes an invoked Excel callback from an unavailable backend.
#[doc(hidden)]
#[allow(
    clippy::large_enum_variant,
    reason = "the invoked raw value is ABI scratch and must not allocate"
)]
pub enum RawCallback {
    NotInvoked(i32),
    Invoked {
        status: i32,
        value: xlfn_sys::XLOPER12,
    },
}

/// Native Excel dispatch after the facade has acquired callback admission.
///
/// # Safety
/// The caller retains a live Excel callback session, proves this invocation
/// is currently permitted, supplies live operands, and takes responsibility
/// for releasing any invoked result before the admission permit is dropped.
#[doc(hidden)]
#[inline]
pub unsafe fn dispatch(
    function: i32,
    arguments: &[std::ptr::NonNull<xlfn_sys::XLOPER12>],
) -> RawCallback {
    // SAFETY: the caller owns host admission and the synchronous arguments.
    let (status, value, invoked) =
        unsafe { xlfn_sys::excel12_with_invocation(function, arguments) };
    if invoked {
        RawCallback::Invoked { status, value }
    } else {
        RawCallback::NotInvoked(status)
    }
}

/// Result of a host action whose outcome is checked by the engine.
#[doc(hidden)]
pub use xlfn_engine::HostMutation;
