//! Internal failure-site diagnostic codes.

use super::id::DiagnosticId;

// Some codes are only referenced by platform- or feature-gated paths.
#[allow(
    dead_code,
    reason = "some diagnostic codes are only used by platform- or feature-gated paths"
)]
impl DiagnosticId {
    pub(crate) const ASYNC_SPAWN: Self = Self::from_ascii8(*b"ASYNCSPN");
    pub(crate) const ASYNC_TIME: Self = Self::from_ascii8(*b"ASYNTIME");
    pub(crate) const CACHE_REENTRANT: Self = Self::from_ascii8(*b"CACHEREC");
    pub(crate) const CACHE_TYPE: Self = Self::from_ascii8(*b"CACHETYP");
    pub(crate) const DIAGNOSTICS_CLOSE: Self = Self::from_ascii8(*b"DIAGCLOS");
    pub(crate) const DIAGNOSTICS_FAILURE: Self = Self::from_ascii8(*b"DIAGFAIL");
    pub(crate) const DIAGNOSTICS_PENDING: Self = Self::from_ascii8(*b"DIAGPEND");
    pub(crate) const DIAGNOSTICS_RESET: Self = Self::from_ascii8(*b"DIAGRSET");
    pub(crate) const HANDLE_SLOT: Self = Self::from_ascii8(*b"HANDSLOT");
    pub(crate) const HANDLE_ENTROPY: Self = Self::from_ascii8(*b"HANDRNGF");
    pub(crate) const HANDLE_TOPIC_COLLISION: Self = Self::from_ascii8(*b"HANDRTDC");
    pub(crate) const ASYNC_FEATURE: Self = Self::from_ascii8(*b"ASYNFEAT");
    pub(crate) const FAILURE: Self = Self::from_ascii8(*b"\0\0\0\0FAIL");
    pub(crate) const LEAN_TRACE: Self = Self::from_ascii8(*b"LEANTRCE");
    pub(crate) const OPEN_STATE: Self = Self::from_ascii8(*b"OPENSTAT");
    pub(crate) const LIFECYCLE_THREAD: Self = Self::from_ascii8(*b"LIFETHRD");
    pub(crate) const LIFECYCLE_SLOT: Self = Self::from_ascii8(*b"LIFESLOT");
    pub(crate) const OPEN_ROLLBACK_FAILURE: Self = Self::from_ascii8(*b"OPRBFAIL");
    pub(crate) const OPEN_ROLLBACK_PENDING: Self = Self::from_ascii8(*b"OPRBPEND");
    pub(crate) const QUIESCENCE_FAILURE: Self = Self::from_ascii8(*b"QUIESCEF");
    pub(crate) const REGISTRATION_UNKNOWN: Self = Self::from_ascii8(*b"REGSUNKN");
    pub(crate) const RTD_GIT_QUIESCENCE: Self = Self::from_ascii8(*b"RTD_GITQ");
    pub(crate) const STATE_SCAN: Self = Self::from_ascii8(*b"STATESCA");
    pub(crate) const TEST_RETRY: Self = Self::from_ascii8(*b"TESTRTRY");
    pub(crate) const REGISTRY: Self = Self::from_ascii8(*b"REGISTRY");
    pub(crate) const REGISTRATION_SIGNATURE: Self = Self::from_ascii8(*b"REGSIGNA");
    pub(crate) const STRING_LENGTH: Self = Self::from_ascii8(*b"STRINGLE");
    pub(crate) const HANDLE_CONTEXT: Self = Self::from_ascii8(*b"HANDCTXT");
    pub(crate) const INPUT_FINGERPRINT: Self = Self::from_ascii8(*b"INPFRMPT");
    pub(crate) const HANDLE_PINS: Self = Self::from_ascii8(*b"HANDPINS");
    pub(crate) const HANDLE_OBJECTS: Self = Self::from_ascii8(*b"HANDOBJS");
    pub(crate) const RETURN_REOPEN: Self = Self::from_ascii8(*b"RTNREOPN");
    pub(crate) const RTD_HANDLE: Self = Self::from_ascii8(*b"RTDHANDL");
    pub(crate) const RTD_MULTI: Self = Self::from_ascii8(*b"RTDMULTI");
    pub(crate) const RTD_DISPATCH: Self = Self::from_ascii8(*b"RTDDISPT");
    pub(crate) const GIT_NULL: Self = Self::from_ascii8(*b"GIT_NULL");
    pub(crate) const ATTEMPT_OVERFLOW: Self = Self::from_ascii8(*b"ATTMOVFL");
    pub(crate) const ATTEMPT_ZERO: Self = Self::from_ascii8(*b"ATTMZERO");
    pub(crate) const CLOSE_LEASE_GATE: Self = Self::from_ascii8(*b"CLLOSEGE");
    pub(crate) const CLOSE_CERTIFICATE: Self = Self::from_ascii8(*b"CLOSECER");
    pub(crate) const CLOSE_RUNTIME: Self = Self::from_ascii8(*b"CLOSERUN");
    pub(crate) const CLOSE_GHOST: Self = Self::from_ascii8(*b"CLOSTGHO");
    pub(crate) const CLOSE_RTD_SUBSCRIPTION: Self = Self::from_ascii8(*b"CLOSTRSU");
    pub(crate) const CLOSE_WAIT: Self = Self::from_ascii8(*b"CLOSWTNO");
    pub(crate) const GHOST_GENERATION: Self = Self::from_ascii8(*b"GHOSTGEN");
    pub(crate) const MISSING_STATE: Self = Self::from_ascii8(*b"MISSSTAT");
    pub(crate) const OPEN_PHASE: Self = Self::from_ascii8(*b"OPENPHAS");
    pub(crate) const MODULE_RESIDENCY: Self = Self::from_ascii8(*b"MODRESID");
    pub(crate) const OPEN_ROLLBACK_CERTIFICATE: Self = Self::from_ascii8(*b"OPRBCERT");
    pub(crate) const OPEN_ROLLBACK_CERT_UNKNOWN: Self = Self::from_ascii8(*b"OPRBCERU");
    pub(crate) const OPEN_ROLLBACK_PHASE: Self = Self::from_ascii8(*b"OPRBPHAS");
    pub(crate) const RTD_SUBSCRIPTION_OVERFLOW: Self = Self::from_ascii8(*b"RTDSUBOV");
    pub(crate) const RTD_SLOTS: Self = Self::from_ascii8(*b"RTDSLOTS");
    pub(crate) const TICKET_OVERFLOW: Self = Self::from_ascii8(*b"TICKOVFL");
    pub(crate) const RTD_INDEX_DUPLICATE: Self = Self::from_ascii8(*b"RTDIDXDU");
    pub(crate) const RTD_RT_ID_OVERFLOW: Self = Self::from_ascii8(*b"RTDRTIDO");
    pub(crate) const RTD_SUBSCRIPTION_ID_OVERFLOW: Self = Self::from_ascii8(*b"RTDSIDOV");
    pub(crate) const ACTIVE_KEY_DUPLICATE: Self = Self::from_ascii8(*b"ACTVKEYD");
    pub(crate) const CONNECTION_INFLIGHT: Self = Self::from_ascii8(*b"CONNINFL");
    pub(crate) const PANIC_SOURCE: Self = Self::from_ascii8(*b"PANICSRC");
    pub(crate) const PANIC_SUBSCRIPTION: Self = Self::from_ascii8(*b"PANICSUB");
    pub(crate) const RESERVATION_OVERFLOW: Self = Self::from_ascii8(*b"RESVOVFL");
    pub(crate) const RTD_INDEX_ORPHAN: Self = Self::from_ascii8(*b"RTDIDXOR");
    pub(crate) const RTD_SERVER_DUE: Self = Self::from_ascii8(*b"RTDSRVDU");
    pub(crate) const SERVER_GENERATION_MISMATCH: Self = Self::from_ascii8(*b"SRVGENMI");
    pub(crate) const NO_REFERENCE: Self = Self::from_ascii8(*b"NOREFRAC");
    pub(crate) const OVERLAPPED_REFERENCE: Self = Self::from_ascii8(*b"OVLPREFR");
    pub(crate) const PANIC_DISCONNECT: Self = Self::from_ascii8(*b"PANICDIS");
    pub(crate) const PANIC_NOTIFY: Self = Self::from_ascii8(*b"PANICNOT");
    pub(crate) const REFERENCE_OVERFLOW: Self = Self::from_ascii8(*b"REFOVFLW");
    pub(crate) const REFERENCE_ID_MISMATCH: Self = Self::from_ascii8(*b"REFRIDMI");
    pub(crate) const TOPIC_ID_DUPLICATE: Self = Self::from_ascii8(*b"TOPICIDD");
    pub(crate) const TOPIC_KEY_DUPLICATE: Self = Self::from_ascii8(*b"TOPICKEY");
    pub(crate) const GRID_INDEX: Self = Self::from_ascii8(*b"GRIDINDX");
    pub(crate) const HANDLE_NO_CONTEXT: Self = Self::from_ascii8(*b"HANDNOCT");
    pub(crate) const RTD_WINDOW_STATUS: Self = Self::from_ascii8(*b"RTDW\0\0\0\0");
    pub(crate) const RTD_WINDOW_FAILURE: Self = Self::from_ascii8(*b"RTDWFAIL");
    pub(crate) const RTD_SERVER_GENERATION_EXHAUSTED: Self = Self::from_ascii8(*b"SRVGENEX");
    pub(crate) const TEST_SENTINEL: Self = Self::from_u64(0xDEAD_BEEF);
}
