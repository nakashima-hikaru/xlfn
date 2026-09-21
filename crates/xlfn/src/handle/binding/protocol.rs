//! Authorization must precede publication access and pointer dereference.
macro_rules! read_binding {
    ($record:ident, $observed:ident;
     $authorized:expr, $foreign:expr, $load:expr, $missing:expr,
     $borrow:expr, $valid:expr, $stale:expr, $accept:expr) => {{
        if !$authorized {
            $foreign;
        }
        let $record = match $load {
            Some(record) => record,
            None => {
                $missing;
            }
        };
        let $observed = $borrow;
        if !$valid {
            $stale;
        }
        $accept
    }};
}
pub(crate) use read_binding;

// Writers retain exclusive publication authority between the check and store.
macro_rules! publish_binding {
    ($empty:expr, $occupied:expr, $publish:expr) => {{
        if !$empty {
            $occupied;
        }
        $publish
    }};
}
pub(crate) use publish_binding;
