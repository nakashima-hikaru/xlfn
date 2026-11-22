use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use std::hash::Hash;
use std::num::NonZeroUsize;

/// Candidate buckets and the number of live identities per distinct source.
pub struct IdentityIndex<Key: IdentityKey, Id> {
    ids_by_key: FxHashMap<Key, SmallVec<[Id; 1]>>,
    source_refs: FxHashMap<Key::Source, NonZeroUsize>,
}

pub trait IdentityKey: Copy + Eq + Hash {
    type Source: Copy + Eq + Hash;

    fn source(self) -> Self::Source;
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum IndexError {
    Duplicate,
    Overflow,
    SourceLimit,
}

impl<Key: IdentityKey, Id> Default for IdentityIndex<Key, Id> {
    fn default() -> Self {
        Self {
            ids_by_key: FxHashMap::default(),
            source_refs: FxHashMap::default(),
        }
    }
}

impl<Key: IdentityKey, Id: Copy + Eq> IdentityIndex<Key, Id> {
    pub fn candidates(&self, key: Key) -> &[Id] {
        self.ids_by_key.get(&key).map_or(&[], SmallVec::as_slice)
    }

    pub fn prepare_insert(
        &mut self,
        key: Key,
        id: Id,
        max_source_ids: usize,
    ) -> Result<IdentityInsertion<'_, Key, Id>, IndexError> {
        let source_count = self.source_refs.len();
        let bucket = self.ids_by_key.entry(key);
        if let std::collections::hash_map::Entry::Occupied(existing) = &bucket
            && existing.get().contains(&id)
        {
            return Err(IndexError::Duplicate);
        }
        let source = match self.source_refs.entry(key.source()) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                let next = entry.get().checked_add(1).ok_or(IndexError::Overflow)?;
                SourceInsertion::Increment { entry, next }
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                if source_count >= max_source_ids {
                    return Err(IndexError::SourceLimit);
                }
                SourceInsertion::New(entry)
            }
        };
        Ok(IdentityInsertion { bucket, source, id })
    }

    pub fn insert(&mut self, key: Key, id: Id, max_source_ids: usize) -> Result<(), IndexError> {
        self.prepare_insert(key, id, max_source_ids)?.commit();
        Ok(())
    }

    pub fn remove(&mut self, key: Key, id: Id) -> bool {
        let Some(ids) = self.ids_by_key.get_mut(&key) else {
            return false;
        };
        let Some(position) = ids.iter().position(|&candidate| candidate == id) else {
            return false;
        };
        ids.swap_remove(position);
        if ids.is_empty() {
            self.ids_by_key.remove(&key);
        }
        release_ref(&mut self.source_refs, key.source());
        true
    }

    pub fn buckets(&self) -> &FxHashMap<Key, SmallVec<[Id; 1]>> {
        &self.ids_by_key
    }

    pub fn clear(&mut self) {
        self.ids_by_key.clear();
        self.source_refs.clear();
    }

    pub fn source_ref_count(&self, source_id: Key::Source) -> Option<NonZeroUsize> {
        self.source_refs.get(&source_id).copied()
    }

    pub fn distinct_source_count(&self) -> usize {
        self.source_refs.len()
    }

    pub fn assert_invariants(&self) {
        let mut expected_source_refs = FxHashMap::default();
        for (key, ids) in &self.ids_by_key {
            assert!(!ids.is_empty());
            *expected_source_refs.entry(key.source()).or_insert(0usize) += ids.len();
        }
        assert_eq!(expected_source_refs.len(), self.source_refs.len());
        for (source_id, refs) in expected_source_refs {
            assert_eq!(
                self.source_refs.get(&source_id).map(|value| value.get()),
                Some(refs)
            );
        }
    }
}

enum SourceInsertion<'index, Source> {
    New(std::collections::hash_map::VacantEntry<'index, Source, NonZeroUsize>),
    Increment {
        entry: std::collections::hash_map::OccupiedEntry<'index, Source, NonZeroUsize>,
        next: NonZeroUsize,
    },
}

/// Native entry borrows retain both exact maps through commit. Callers cannot
/// substitute a source, reuse an increment plan or commit against another index.
pub struct IdentityInsertion<'index, Key: IdentityKey, Id> {
    bucket: std::collections::hash_map::Entry<'index, Key, SmallVec<[Id; 1]>>,
    source: SourceInsertion<'index, Key::Source>,
    id: Id,
}
impl<Key: IdentityKey, Id: Copy + Eq> IdentityInsertion<'_, Key, Id> {
    pub fn commit(self) {
        self.bucket.or_default().push(self.id);
        match self.source {
            SourceInsertion::New(entry) => {
                entry.insert(NonZeroUsize::MIN);
            }
            SourceInsertion::Increment { mut entry, next } => {
                *entry.get_mut() = next;
            }
        }
    }
}

fn release_ref<Source: Copy + Eq + Hash>(
    refs: &mut FxHashMap<Source, NonZeroUsize>,
    source_id: Source,
) {
    let count = refs
        .get_mut(&source_id)
        .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());

    if count.get() == 1 {
        refs.remove(&source_id);
    } else {
        *count = NonZeroUsize::new(count.get() - 1)
            .expect("a source refcount greater than one remains non-zero");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Hash, Eq, PartialEq)]
    struct Key(u8, u64);

    impl IdentityKey for Key {
        type Source = u8;

        fn source(self) -> Self::Source {
            self.0
        }
    }

    #[test]
    fn index_admission_is_affine_per_bucket_and_counts_live_sources() {
        let mut index = IdentityIndex::default();
        let one = Key(1, 10);
        let two = Key(2, 10);
        {
            let _abandoned = index.prepare_insert(one, 1, 1).unwrap();
        }
        assert_eq!(index.distinct_source_count(), 0);
        assert!(index.candidates(one).is_empty());
        index.insert(one, 1, 1).unwrap();
        index.insert(one, 2, 1).unwrap();
        assert!(matches!(
            index.prepare_insert(one, 2, 1),
            Err(IndexError::Duplicate)
        ));
        assert!(matches!(
            index.prepare_insert(two, 3, 1),
            Err(IndexError::SourceLimit)
        ));
        assert_eq!(index.source_ref_count(1).map(NonZeroUsize::get), Some(2));
        assert_eq!(index.candidates(one), [1, 2]);
        assert!(!index.remove(one, 3));
        assert!(index.remove(one, 1));
        assert!(index.remove(one, 2));
        assert_eq!(index.distinct_source_count(), 0);
        index.insert(two, 3, 1).unwrap();
        index.assert_invariants();
    }
}
