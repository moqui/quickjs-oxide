//! A string result built as bytes while every unit fits in one, else as
//! UTF-16: a Latin-1 result is never widened and narrowed again, and a
//! widened one holds a unit past Latin-1, so it is canonical UTF-16 with no
//! narrowing scan.

use crate::heap::string::StringRepr;
use crate::value::{StringId, Value};
use crate::vm::error;
use crate::vm::{Cx, Throw};

pub(crate) enum Joined {
    Bytes(Vec<u8>),
    /// Holds at least one unit past `0xFF`.
    Units(Vec<u16>),
}

impl Joined {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Joined::Bytes(Vec::with_capacity(capacity))
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            Joined::Bytes(bytes) => bytes.len(),
            Joined::Units(units) => units.len(),
        }
    }

    fn widen(&mut self) -> &mut Vec<u16> {
        if let Joined::Bytes(bytes) = self {
            let mut units = Vec::with_capacity(bytes.capacity().max(bytes.len() * 2));
            units.extend(bytes.iter().map(|&byte| u16::from(byte)));
            *self = Joined::Units(units);
        }
        match self {
            Joined::Units(units) => units,
            Joined::Bytes(_) => unreachable!("widened above"),
        }
    }

    pub(crate) fn push_bytes(&mut self, part: &[u8]) {
        match self {
            Joined::Bytes(bytes) => bytes.extend_from_slice(part),
            Joined::Units(units) => units.extend(part.iter().map(|&byte| u16::from(byte))),
        }
    }

    #[inline]
    pub(crate) fn push_byte(&mut self, byte: u8) {
        match self {
            Joined::Bytes(bytes) => bytes.push(byte),
            Joined::Units(units) => units.push(u16::from(byte)),
        }
    }

    /// The units another builder holds.
    pub(crate) fn push_joined(&mut self, other: &Joined) {
        match other {
            Joined::Bytes(bytes) => self.push_bytes(bytes),
            Joined::Units(units) => self.widen().extend_from_slice(units),
        }
    }

    /// Drops the units past `len`; a widened builder left with no unit past
    /// `0xFF` is narrowed again, so it stays canonical.
    pub(crate) fn truncate(&mut self, len: usize) {
        match self {
            Joined::Bytes(bytes) => bytes.truncate(len),
            Joined::Units(units) => {
                units.truncate(len);
                if units.iter().all(|&unit| unit <= 0xFF) {
                    *self = Joined::Bytes(units.iter().map(|&unit| unit as u8).collect());
                }
            }
        }
    }

    /// `count` copies of the Latin-1 unit `byte`.
    pub(crate) fn push_byte_run(&mut self, byte: u8, count: usize) {
        match self {
            Joined::Bytes(bytes) => bytes.resize(bytes.len() + count, byte),
            Joined::Units(units) => units.resize(units.len() + count, u16::from(byte)),
        }
    }

    pub(crate) fn push_units(&mut self, part: &[u16]) {
        if let Joined::Bytes(bytes) = self
            && part.iter().all(|&unit| unit <= 0xFF)
        {
            bytes.extend(part.iter().map(|&unit| unit as u8));
            return;
        }
        self.widen().extend_from_slice(part);
    }

    /// The units of `repr`, copied from where they lie.
    pub(crate) fn push_repr(&mut self, repr: &StringRepr) {
        match repr {
            StringRepr::OneByte(bytes) => self.push_bytes(bytes),
            // A canonical two-byte string holds a unit past `0xFF`.
            StringRepr::TwoByte(units) => self.widen().extend_from_slice(units),
        }
    }

    /// Units `range` of the string `id`, copied from where they lie.
    pub(crate) fn push_subject(
        &mut self,
        cx: &Cx<'_>,
        id: StringId,
        range: std::ops::Range<usize>,
    ) {
        match cx.heap.string(id).repr() {
            StringRepr::OneByte(bytes) => self.push_bytes(&bytes[range]),
            StringRepr::TwoByte(units) => self.push_units(&units[range]),
        }
    }

    pub(crate) fn push_string(&mut self, cx: &Cx<'_>, id: StringId) {
        self.push_repr(cx.heap.string(id).repr());
    }

    /// The string value, with no scan of its units; a result over the
    /// string limit is a `RangeError`.
    pub(crate) fn finish(self, cx: &mut Cx<'_>) -> Result<Value, Throw> {
        if self.len() > super::string::MAX_STRING {
            return Err(error::string_too_long(cx));
        }
        let repr = match self {
            Joined::Bytes(bytes) => StringRepr::OneByte(bytes.into()),
            Joined::Units(units) => StringRepr::TwoByte(units.into()),
        };
        super::string_repr(cx, repr)
    }
}
