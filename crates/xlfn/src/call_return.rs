//! Call-scoped return dispatch and semantic payload construction.

mod collections;
mod context;
#[cfg(feature = "handles")]
mod handles;
mod payload;
mod rtd;
mod traits;

pub use context::ReturnContext;
pub use payload::ReturnPayload;
pub use traits::{
    AsyncReturn, ExcelReturn, ExcelReturnSealed, MacroSheetReturn, MainThreadReturn,
    ThreadSafeReturn, VolatileReturn,
};

#[doc(hidden)]
pub use traits::{
    assert_async_parameter, assert_async_return, assert_macro_sheet_return,
    assert_main_thread_return, assert_thread_safe_return, assert_volatile_return,
};
