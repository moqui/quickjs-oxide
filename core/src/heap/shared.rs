//! Atoms shared by every runtime of the process (`docs/shared-atoms.md`).
//!
//! A shared atom is immutable and immortal: its text, its content hash and
//! the array index it spells, if any. Its id is a `StringId` at or above
//! [`SHARED_BASE`], the same in every runtime, so a property key, a value and
//! a unit's key table name it as they name a runtime's own atom. A runtime's
//! string arena stays below that base.
//!
//! The table is the embedder's ([`SharedAtoms`]): the core has no lock of
//! its own. The std embedding API installs the process's table
//! (`quickjs_oxide::engine::api::core`'s `ProcessAtoms`) on every runtime it
//! makes, through [`crate::host::Host::shared_atoms`]; a core runtime made
//! without one keeps every atom to itself.

use alloc::boxed::Box;
use alloc::sync::Arc;

use super::string::StringRepr;
use crate::heap::atom::PropertyKey;
use crate::value::StringId;

/// The first shared id. A runtime's string arena gives out indices below it.
pub const SHARED_BASE: u32 = 1 << 29;

/// Records in a table's first segment, when it keeps them in segments that
/// double: segment `k` holds `FIRST << k`.
pub const FIRST: u32 = 1024;
/// The segments of such a table.
pub const SEGMENTS_LEN: usize = 8;
/// The most shared atoms: all of the segments. Past it a runtime keeps its
/// own.
pub const CAPACITY: u32 = FIRST * ((1 << SEGMENTS_LEN) - 1);
/// The most UTF-16 units of the shared atoms' texts together.
pub const CAPACITY_UNITS: usize = 16 << 20;

/// The segment and the offset in it of shared record `offset`, for a table
/// laid out in [`FIRST`]-based doubling segments.
#[inline]
pub fn locate(offset: u32) -> (usize, usize) {
    let biased = offset + FIRST;
    let segment = (31 - biased.leading_zeros() - FIRST.trailing_zeros()) as usize;
    (segment, (biased - (FIRST << segment)) as usize)
}

/// An immutable atom.
pub struct SharedAtom {
    repr: StringRepr,
    hash: u32,
    array_index: u32,
}

impl SharedAtom {
    /// The atom of `repr`, whose content hash is `hash`.
    pub fn new(repr: StringRepr, hash: u32) -> Self {
        let array_index = repr.array_index().unwrap_or(super::object::NONE);
        SharedAtom {
            repr,
            hash: hash.max(1),
            array_index,
        }
    }

    #[inline]
    pub fn repr(&self) -> &StringRepr {
        &self.repr
    }

    /// The content hash, never 0 ([`super::string::StringRecord::hash`]).
    #[inline]
    pub fn hash(&self) -> u32 {
        self.hash
    }

    #[inline]
    pub fn array_index(&self) -> u32 {
        self.array_index
    }
}

/// The id of shared atom `index`, for a table's [`SharedAtoms`] answers.
#[inline]
pub const fn id(index: u32) -> StringId {
    debug_assert!(is_shared(index), "a shared atom's index");
    StringId(index)
}

/// Whether `index` (a string index) names a shared atom.
#[inline(always)]
pub const fn is_shared(index: u32) -> bool {
    index >= SHARED_BASE
}

/// The shared table's size: its atoms, their texts' units, and the bytes
/// the process holds for it (the segments made, the texts, the index).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SharedStats {
    pub atoms: usize,
    pub units: usize,
    pub bytes: usize,
}

/// The process's table of shared atoms. Every runtime that shares code or
/// keys with another holds the same table. Ids are [`SHARED_BASE`] plus the
/// order of insertion, and an atom once added is never removed.
pub trait SharedAtoms: Sync {
    /// The shared atom `index`; it exists (its id came from this table).
    fn get(&self, index: u32) -> &SharedAtom;

    /// The shared atom with the content of `repr`, whose content hash is
    /// `hash`.
    fn find(&self, repr: &StringRepr, hash: u32) -> Option<StringId>;

    /// The shared atom spelled by the Latin-1 `bytes`, whose content hash is
    /// `hash`.
    fn find_one_byte(&self, bytes: &[u8], hash: u32) -> Option<StringId>;

    /// The shared atom with the content of `repr`, added if it is new;
    /// `repr` back once the table is full ([`CAPACITY`], [`CAPACITY_UNITS`]).
    fn find_or_insert(&self, repr: StringRepr, hash: u32) -> Result<StringId, StringRepr>;

    fn stats(&self) -> SharedStats;

    /// A new slot for a code image unit's key table, which every runtime
    /// holding this table shares once one has set it.
    fn key_slot(&self) -> Box<dyn KeySlot>;
}

/// A key table set once and read from any thread ([`SharedAtoms::key_slot`]).
pub trait KeySlot: Send + Sync {
    fn get(&self) -> Option<&Arc<[PropertyKey]>>;

    /// The table set, after setting it to `keys` when none is.
    fn get_or_init(&self, keys: Arc<[PropertyKey]>) -> Arc<[PropertyKey]>;
}

/// No table: every atom is a runtime's own.
pub(crate) struct NoShared;

pub(crate) static NO_SHARED: NoShared = NoShared;

impl SharedAtoms for NoShared {
    fn get(&self, _index: u32) -> &SharedAtom {
        unreachable!("no shared atoms without a table")
    }

    fn find(&self, _repr: &StringRepr, _hash: u32) -> Option<StringId> {
        None
    }

    fn find_one_byte(&self, _bytes: &[u8], _hash: u32) -> Option<StringId> {
        None
    }

    fn find_or_insert(&self, repr: StringRepr, _hash: u32) -> Result<StringId, StringRepr> {
        Err(repr)
    }

    fn stats(&self) -> SharedStats {
        SharedStats::default()
    }

    fn key_slot(&self) -> Box<dyn KeySlot> {
        Box::new(NoSlot)
    }
}

/// A slot that never holds a table: without shared atoms no key table is
/// the same in two runtimes.
struct NoSlot;

impl KeySlot for NoSlot {
    fn get(&self) -> Option<&Arc<[PropertyKey]>> {
        None
    }

    fn get_or_init(&self, keys: Arc<[PropertyKey]>) -> Arc<[PropertyKey]> {
        keys
    }
}

/// A table for the core's own tests, with std's locks: the embedding API's
/// `ProcessAtoms` is the one runtimes use.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use alloc::vec::Vec;
    use std::sync::{Mutex, OnceLock};

    #[derive(Default)]
    pub(crate) struct TestAtoms {
        atoms: Mutex<Vec<&'static SharedAtom>>,
    }

    /// The tests' table, one for the test process.
    pub(crate) fn table() -> &'static TestAtoms {
        static TABLE: OnceLock<TestAtoms> = OnceLock::new();
        TABLE.get_or_init(TestAtoms::default)
    }

    impl SharedAtoms for TestAtoms {
        fn get(&self, index: u32) -> &SharedAtom {
            self.atoms.lock().unwrap()[(index - SHARED_BASE) as usize]
        }

        fn find(&self, repr: &StringRepr, hash: u32) -> Option<StringId> {
            let atoms = self.atoms.lock().unwrap();
            atoms
                .iter()
                .position(|atom| atom.hash == hash.max(1) && atom.repr == *repr)
                .map(|offset| StringId(SHARED_BASE + offset as u32))
        }

        fn find_one_byte(&self, bytes: &[u8], hash: u32) -> Option<StringId> {
            self.find(&StringRepr::OneByte(bytes.into()), hash)
        }

        fn find_or_insert(&self, repr: StringRepr, hash: u32) -> Result<StringId, StringRepr> {
            let mut atoms = self.atoms.lock().unwrap();
            if let Some(offset) = atoms
                .iter()
                .position(|atom| atom.hash == hash.max(1) && atom.repr == repr)
            {
                return Ok(StringId(SHARED_BASE + offset as u32));
            }
            let id = SHARED_BASE + atoms.len() as u32;
            atoms.push(Box::leak(Box::new(SharedAtom::new(repr, hash))));
            Ok(StringId(id))
        }

        fn stats(&self) -> SharedStats {
            let atoms = self.atoms.lock().unwrap();
            SharedStats {
                atoms: atoms.len(),
                units: atoms.iter().map(|atom| atom.repr.len()).sum(),
                bytes: 0,
            }
        }

        fn key_slot(&self) -> Box<dyn KeySlot> {
            Box::new(TestSlot::default())
        }
    }

    #[derive(Default)]
    struct TestSlot(OnceLock<Arc<[PropertyKey]>>);

    impl KeySlot for TestSlot {
        fn get(&self) -> Option<&Arc<[PropertyKey]>> {
            self.0.get()
        }

        fn get_or_init(&self, keys: Arc<[PropertyKey]>) -> Arc<[PropertyKey]> {
            Arc::clone(self.0.get_or_init(|| keys))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::table;
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn segments_cover_the_offsets_in_order() {
        assert_eq!(locate(0), (0, 0));
        assert_eq!(locate(FIRST - 1), (0, FIRST as usize - 1));
        assert_eq!(locate(FIRST), (1, 0));
        assert_eq!(locate(3 * FIRST - 1), (1, 2 * FIRST as usize - 1));
        assert_eq!(locate(3 * FIRST), (2, 0));
        assert_eq!(locate(CAPACITY - 1).0, SEGMENTS_LEN - 1);
    }

    /// Without a table every atom is the heap's own.
    #[test]
    fn a_heap_without_a_table_keeps_its_atoms() {
        let mut heap = super::super::Heap {
            share_atoms: true,
            ..Default::default()
        };
        let id = heap.intern("shared-atoms-none").unwrap();
        assert!(!is_shared(id.0));
    }

    fn heap(share_atoms: bool) -> super::super::Heap {
        let mut heap = super::super::Heap {
            share_atoms,
            ..Default::default()
        };
        heap.strings.set_shared(table());
        heap
    }

    /// A runtime that made its own atom for a text before the text was
    /// shared keeps using its own; a runtime after finds the shared one.
    #[test]
    fn a_text_has_one_id_in_each_heap() {
        let mut early = heap(false);
        let own = early.intern("shared-atoms-order").unwrap();
        assert!(!is_shared(own.0));
        let mut sharing = heap(true);
        let shared = sharing.intern("shared-atoms-order").unwrap();
        assert!(is_shared(shared.0));
        assert_eq!(early.intern("shared-atoms-order").unwrap(), own);
        let mut late = heap(false);
        assert_eq!(late.intern("shared-atoms-order").unwrap(), shared);
        assert_eq!(late.intern_latin1(b"shared-atoms-order").unwrap(), shared);
        let units = "shared-atoms-order".encode_utf16().collect::<Vec<_>>();
        assert_eq!(late.intern_units(&units).unwrap(), shared);
        // A shared atom outlives every collection of a heap that holds it,
        // and one that does not.
        let object = late
            .alloc_object(crate::ObjectKind::Ordinary, None, 1)
            .unwrap();
        late.add_fixed_root(crate::Value::object(object));
        late.add_property(
            object,
            crate::PropertyKey::atom(shared),
            crate::PropertyFlags::DEFAULT,
            crate::Value::string(shared),
        )
        .unwrap();
        late.collect();
        late.collect();
        assert_eq!(
            late.get_property(object, crate::PropertyKey::atom(shared)),
            Some(crate::Value::string(shared))
        );
        assert_eq!(late.string_text(shared), "shared-atoms-order");
        assert!(late.string(shared).is_atom());
        assert_eq!(late.intern("shared-atoms-order").unwrap(), shared);
    }
}
