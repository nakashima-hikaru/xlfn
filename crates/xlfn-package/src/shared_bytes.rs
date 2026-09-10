use std::ops::Deref;
use std::sync::Arc;

/// Immutable bytes shared between snapshot, verification and commit stages.
///
/// Taking ownership of a vector preserves its existing allocation. Cloning
/// shares that allocation; no mutable access to the vector is exposed.
#[derive(Clone, Debug)]
pub struct SharedBytes(Arc<Vec<u8>>);

impl From<Vec<u8>> for SharedBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self(Arc::new(bytes))
    }
}

impl AsRef<[u8]> for SharedBytes {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl Deref for SharedBytes {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::SharedBytes;

    #[test]
    fn taking_ownership_and_cloning_preserve_the_original_buffer() {
        let mut original = Vec::with_capacity(4096);
        original.extend_from_slice(b"stable artifact bytes");
        let pointer = original.as_ptr();
        let bytes = SharedBytes::from(original);
        assert_eq!(bytes.as_ptr(), pointer);
        assert_eq!(bytes.as_ref(), b"stable artifact bytes");
        let cloned = bytes.clone();
        assert_eq!(cloned.as_ptr(), pointer);
        drop(bytes);
        assert_eq!(cloned.as_ptr(), pointer);
        assert_eq!(cloned.as_ref(), b"stable artifact bytes");
    }
}
