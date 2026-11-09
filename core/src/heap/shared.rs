//! Atoms shared by every runtime of the process (`docs/shared-atoms.md`).
//!
//! A shared atom is immutable and immortal: its text, its content hash and
//! the array index it spells, if any. Its id is a `StringId` at or above
//! [`SHARED_BASE`], the same in every runtime, so a property key, a value and
//! a unit's key table name it as they name a runtime's own atom. A runtime's
//! string arena stays below that base.
//!
//! The records live in segments that are made once and never move: segment
//! `k` holds `FIRST << k` of them, so a record is found by its id with no
//! lock. Finding an atom by its text, and adding one, take the table's
//! mutex; a runtime does that once per text, and then finds the id in its own
//! atom table, where it imports it ([`super::Heap`]'s interning).

use std::sync::{Mutex, OnceLock};

use super::hash::FxHashMap;
use super::string::StringRepr;
use crate::value::StringId;

/// The first shared id. A runtime's string arena gives out indices below it.
pub const SHARED_BASE: u32 = 1 << 29;

/// Records in segment 0; segment `k` holds `FIRST << k`.
const FIRST: u32 = 1024;
const SEGMENTS_LEN: usize = 8;
/// The most shared atoms: all of the segments. Past it a runtime keeps its
/// own.
pub const CAPACITY: u32 = FIRST * ((1 << SEGMENTS_LEN) - 1);
/// The most UTF-16 units of the shared atoms' texts together.
pub const CAPACITY_UNITS: usize = 16 << 20;

/// An immutable atom.
pub struct SharedAtom {
    repr: StringRepr,
    hash: u32,
    array_index: u32,
}

impl SharedAtom {
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
static INDEX: std::sync::LazyLock<Mutex<Index>> = std::sync::LazyLock::new(|| {
    Mutex::new(Index {
        by_hash: FxHashMap::default(),
        len: 0,
        units: 0,
    })
});

/// The segment and the offset in it of shared record `offset`.
#[inline]
fn locate(offset: u32) -> (usize, usize) {
    let biased = offset + FIRST;
    let segment = (31 - biased.leading_zeros() - FIRST.trailing_zeros()) as usize;
    (segment, (biased - (FIRST << segment)) as usize)
}

/// Whether `index` (a string index) names a shared atom.
#[inline(always)]
pub const fn is_shared(index: u32) -> bool {
    index >= SHARED_BASE
}

/// The shared atom `index`; it exists (its id came from this table).
#[inline]
pub fn get(index: u32) -> &'static SharedAtom {
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
        .map(StringId)
}

fn lock() -> std::sync::MutexGuard<'static, Index> {
    // The index is consistent after any panic: an insert pushes its id last.
    INDEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The shared atom spelled by the Latin-1 `bytes`, whose content hash is
/// `hash`.
pub fn find_one_byte(bytes: &[u8], hash: u32) -> Option<StringId> {
    let index = lock();
    find_in(
        &index,
        |repr| matches!(repr, StringRepr::OneByte(atom) if **atom == *bytes),
        hash,
    )
}

/// The shared atom with the content of `repr`, whose content hash is `hash`.
pub fn find(repr: &StringRepr, hash: u32) -> Option<StringId> {
    let index = lock();
    find_in(&index, |atom| atom == repr, hash)
}

/// The shared atom with the content of `repr`, added if it is new; `repr`
/// back once the table is full.
pub fn find_or_insert(repr: StringRepr, hash: u32) -> Result<StringId, StringRepr> {
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
    let array_index = repr.array_index().unwrap_or(super::object::NONE);
    index.units += repr.len();
    let _ = records[slot].set(SharedAtom {
        repr,
        hash: hash.max(1),
        array_index,
    });
    index.len += 1;
    let id = SHARED_BASE + offset;
    index.by_hash.entry(hash).or_default().push(id);
    Ok(StringId(id))
}

/// The shared table's size: its atoms, their texts' units, and the bytes
/// the process holds for it (the segments made, the texts, the index).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SharedStats {
    pub atoms: usize,
    pub units: usize,
    pub bytes: usize,
}

pub fn stats() -> SharedStats {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_cover_the_offsets_in_order() {
        assert_eq!(locate(0), (0, 0));
        assert_eq!(locate(FIRST - 1), (0, FIRST as usize - 1));
        assert_eq!(locate(FIRST), (1, 0));
        assert_eq!(locate(3 * FIRST - 1), (1, 2 * FIRST as usize - 1));
        assert_eq!(locate(3 * FIRST), (2, 0));
        assert_eq!(locate(CAPACITY - 1).0, SEGMENTS_LEN - 1);
    }

    #[test]
    fn an_atom_is_added_once_and_found_by_its_text() {
        let repr = StringRepr::from_text("shared-atoms-test-key");
        let hash = repr.content_hash();
        let id = find_or_insert(repr.clone(), hash).unwrap();
        assert!(is_shared(id.0));
        assert_eq!(find_or_insert(repr.clone(), hash), Ok(id));
        assert_eq!(find(&repr, hash), Some(id));
        assert_eq!(find_one_byte(b"shared-atoms-test-key", hash), Some(id));
        assert_eq!(get(id.0).repr(), &repr);
        assert_eq!(get(id.0).array_index(), crate::heap::object::NONE);
    }

    /// A runtime that made its own atom for a text before the text was
    /// shared keeps using its own; a runtime after finds the shared one.
    #[test]
    fn a_text_has_one_id_in_each_heap() {
        let mut early = super::super::Heap::default();
        let own = early.intern("shared-atoms-order").unwrap();
        assert!(!is_shared(own.0));
        let mut sharing = super::super::Heap {
            share_atoms: true,
            ..Default::default()
        };
        let shared = sharing.intern("shared-atoms-order").unwrap();
        assert!(is_shared(shared.0));
        assert_eq!(early.intern("shared-atoms-order").unwrap(), own);
        let mut late = super::super::Heap::default();
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

    #[test]
    fn threads_adding_the_same_texts_get_one_id_each() {
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
                                find_or_insert(repr, hash).unwrap()
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
            assert_eq!(get(id.0).repr().to_rust_string(), *text);
        }
    }
}
