//! The process's shared atoms ([`oxide_core::heap::shared`]): the table every
//! runtime the embedding API makes holds, so a key or a code image's key
//! table names the same atom in each of them (`docs/shared-atoms.md`).
//!
//! The records live in segments that are made once and never move: segment
//! `k` holds `FIRST << k` of them, so a record is found by its id with no
//! lock. Finding an atom by its text, and adding one, take the table's
//! mutex; a runtime does that once per text, and then finds the id in its own
//! atom table, where it imports it.

use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock, PoisonError};

use oxide_core::FxHashMap;
use oxide_core::PropertyKey;
use oxide_core::heap::shared::{
    self, CAPACITY, CAPACITY_UNITS, FIRST, KeySlot, SEGMENTS_LEN, SHARED_BASE, SharedAtom,
    SharedAtoms, SharedStats, locate,
};
use oxide_core::heap::string::StringRepr;
use oxide_core::value::StringId;

/// The table of the process.
pub(super) struct ProcessAtoms;

impl ProcessAtoms {
    pub(super) fn shared() -> &'static dyn SharedAtoms {
        static SHARED: ProcessAtoms = ProcessAtoms;
        &SHARED
    }
}

struct Index {
    /// Content hash → ids with that hash.
    by_hash: FxHashMap<u32, Vec<u32>>,
    len: u32,
    units: usize,
}

/// The records, by segment: each segment made once, each record set once.
static SEGMENTS: [OnceLock<Box<[OnceLock<SharedAtom>]>>; SEGMENTS_LEN] =
    [const { OnceLock::new() }; SEGMENTS_LEN];

/// Finding by text and adding.
static INDEX: LazyLock<Mutex<Index>> = LazyLock::new(|| {
    Mutex::new(Index {
        by_hash: FxHashMap::default(),
        len: 0,
        units: 0,
    })
});

#[inline]
fn get(index: u32) -> &'static SharedAtom {
    let (segment, offset) = locate(index - SHARED_BASE);
    SEGMENTS[segment]
        .get()
        .and_then(|records| records.get(offset)?.get())
        .expect("a shared atom")
}

fn find_in(index: &Index, matches: impl Fn(&StringRepr) -> bool, hash: u32) -> Option<StringId> {
    index
        .by_hash
        .get(&hash)?
        .iter()
        .copied()
        .find(|&id| matches(get(id).repr()))
        .map(shared::id)
}

fn lock() -> MutexGuard<'static, Index> {
    // The index is consistent after any panic: an insert pushes its id last.
    INDEX.lock().unwrap_or_else(PoisonError::into_inner)
}

impl SharedAtoms for ProcessAtoms {
    #[inline]
    fn get(&self, index: u32) -> &SharedAtom {
        get(index)
    }

    fn find(&self, repr: &StringRepr, hash: u32) -> Option<StringId> {
        let index = lock();
        find_in(&index, |atom| atom == repr, hash)
    }

    fn find_one_byte(&self, bytes: &[u8], hash: u32) -> Option<StringId> {
        let index = lock();
        find_in(
            &index,
            |repr| matches!(repr, StringRepr::OneByte(atom) if **atom == *bytes),
            hash,
        )
    }

    fn find_or_insert(&self, repr: StringRepr, hash: u32) -> Result<StringId, StringRepr> {
        let mut index = lock();
        if let Some(id) = find_in(&index, |atom| *atom == repr, hash) {
            return Ok(id);
        }
        if index.len >= CAPACITY || index.units + repr.len() > CAPACITY_UNITS {
            return Err(repr);
        }
        let offset = index.len;
        let (segment, slot) = locate(offset);
        let records = SEGMENTS[segment].get_or_init(|| {
            (0..FIRST << segment)
                .map(|_| OnceLock::new())
                .collect::<Box<[_]>>()
        });
        index.units += repr.len();
        let _ = records[slot].set(SharedAtom::new(repr, hash));
        index.len += 1;
        let id = SHARED_BASE + offset;
        index.by_hash.entry(hash).or_default().push(id);
        Ok(shared::id(id))
    }

    fn stats(&self) -> SharedStats {
        let index = lock();
        let segments: usize = SEGMENTS
            .iter()
            .filter_map(OnceLock::get)
            .map(|records| records.len() * size_of::<OnceLock<SharedAtom>>())
            .sum();
        let texts: usize = (0..index.len)
            .map(|offset| get(SHARED_BASE + offset).repr().content_bytes())
            .sum();
        let lists: usize = index
            .by_hash
            .values()
            .map(|ids| ids.capacity() * size_of::<u32>())
            .sum();
        SharedStats {
            atoms: index.len as usize,
            units: index.units,
            bytes: segments
                + texts
                + index.by_hash.capacity() * (size_of::<(u32, Vec<u32>)>() + 1)
                + lists,
        }
    }

    fn key_slot(&self) -> Box<dyn KeySlot> {
        Box::new(Slot::default())
    }
}

/// A code image unit's key table, set by the first load that finds every
/// key a shared atom.
#[derive(Default)]
struct Slot(OnceLock<Arc<[PropertyKey]>>);

impl KeySlot for Slot {
    fn get(&self) -> Option<&Arc<[PropertyKey]>> {
        self.0.get()
    }

    fn get_or_init(&self, keys: Arc<[PropertyKey]>) -> Arc<[PropertyKey]> {
        Arc::clone(self.0.get_or_init(|| keys))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxide_core::heap::shared::is_shared;

    #[test]
    fn an_atom_is_added_once_and_found_by_its_text() {
        let table = ProcessAtoms::shared();
        let repr = StringRepr::from_text("shared-atoms-test-key");
        let hash = repr.content_hash();
        let id = table.find_or_insert(repr.clone(), hash).unwrap();
        assert!(is_shared(id.index()));
        assert_eq!(table.find_or_insert(repr.clone(), hash), Ok(id));
        assert_eq!(table.find(&repr, hash), Some(id));
        assert_eq!(
            table.find_one_byte(b"shared-atoms-test-key", hash),
            Some(id)
        );
        assert_eq!(table.get(id.index()).repr(), &repr);
        assert_eq!(table.get(id.index()).array_index(), u32::MAX);
    }

    #[test]
    fn threads_adding_the_same_texts_get_one_id_each() {
        let table = ProcessAtoms::shared();
        let texts = (0..200)
            .map(|index| format!("shared-atoms-thread-{index}"))
            .collect::<Vec<_>>();
        let ids = std::thread::scope(|scope| {
            let handles = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        texts
                            .iter()
                            .map(|text| {
                                let repr = StringRepr::from_text(text);
                                let hash = repr.content_hash();
                                table.find_or_insert(repr, hash).unwrap()
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        for other in &ids[1..] {
            assert_eq!(other, &ids[0]);
        }
        for (text, id) in texts.iter().zip(&ids[0]) {
            assert_eq!(table.get(id.index()).repr().to_rust_string(), *text);
        }
    }
}
