//! Register bytecode: fixed-width instructions, code units and the store that
//! owns them.
//!
//! A code unit is built once by the lowering, checked once by
//! [`validate`](validate::validate) when it is published, and then run
//! without any decoding: the main loop matches on the typed [`Opcode`] and
//! indexes registers with plain `[]`. Units published before the program
//! runs live in the [`CodeStore`] for the runtime's lifetime; units compiled
//! at run time live in the heap, which frees each once no function, frame
//! or job reaches it.

pub mod builder;
pub mod disasm;
pub mod opcode;
pub mod validate;

use std::fmt;

pub use self::builder::CodeBuilder;
pub use self::disasm::disassemble;
pub use self::opcode::{Format, Opcode};
pub use self::validate::{ValidateError, ValidateErrorKind};
use crate::heap::atom::PropertyKey;

/// One register instruction. `a`, `b`, `c` are register operands or small
/// counts; `imm` is a constant index, an absolute jump target, a key's bits,
/// a scope slot or an `i32` literal, as the opcode's [`Format`] says.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Insn {
    pub op: Opcode,
    pub a: u8,
    pub b: u8,
    pub c: u8,
    pub imm: u32,
}

const _: () = assert!(size_of::<Insn>() == 8);

/// Register operands address a 256-register window; registers above it are
/// reached through `LoadWide` and `StoreWide`.
pub const WINDOW: u32 = 256;

impl Insn {
    #[inline]
    pub const fn new(op: Opcode, a: u8, b: u8, c: u8, imm: u32) -> Insn {
        Insn { op, a, b, c, imm }
    }

    /// The `imm` field read as an `i32` literal.
    #[inline]
    pub const fn int(self) -> i32 {
        self.imm as i32
    }

    /// The `i16` literal of an [`Format::AShortJump`] instruction.
    #[inline]
    pub const fn short(self) -> i16 {
        (self.b as u16 | (self.c as u16) << 8) as i16
    }

    /// The `imm` field read as a property key.
    #[inline]
    pub const fn key(self) -> PropertyKey {
        PropertyKey::from_raw(self.imm)
    }

    /// The cache site of an instruction that has one: `c` for `GetField` and
    /// `SetField`, `b | c << 8` for the global forms, `imm` for `New`. An index
    /// into the unit's entries of the [`IcStore`](crate::vm::ic::IcStore).
    #[inline]
    pub const fn site(self) -> Option<u32> {
        match self.op {
            Opcode::GetField | Opcode::SetField => Some(self.c as u32),
            Opcode::GetGlobal | Opcode::GetGlobalOrUndefined | Opcode::SetGlobal => {
                Some(self.b as u32 | (self.c as u32) << 8)
            }
            Opcode::New => Some(self.imm),
            _ => None,
        }
    }

    /// Whether `imm` is a jump target.
    #[inline]
    pub const fn has_target(self) -> bool {
        matches!(
            self.op.format(),
            Format::Jump | Format::AJump | Format::ABJump | Format::AShortJump
        )
    }
}

impl fmt::Debug for Insn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} a={} b={} c={} imm={:#x}",
            self.op.name(),
            self.a,
            self.b,
            self.c,
            self.imm
        )
    }
}

/// Index of a [`CodeUnit`] in the [`CodeStore`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CodeUnitId(pub(crate) u32);

impl CodeUnitId {
    #[inline]
    pub const fn index(self) -> u32 {
        self.0
    }
}

impl fmt::Debug for CodeUnitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CodeUnitId({})", self.0)
    }
}

/// An exception handler: a throw at an instruction in `start..end` stores the
/// thrown value in `register` (the temporary at the handler's stack depth)
/// and continues at `target`. Entries are ordered innermost first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handler {
    pub start: u32,
    pub end: u32,
    pub target: u32,
    pub register: u16,
}

pub mod flags {
    /// Strict-mode code: `this` is not coerced.
    pub const STRICT: u16 = 1 << 0;
    /// The function may be called with `new`.
    pub const CONSTRUCTOR: u16 = 1 << 1;
    /// An arrow function: `this` is lexical, never a constructor.
    pub const ARROW: u16 = 1 << 2;
    /// A script body.
    pub const SCRIPT: u16 = 1 << 3;
    /// The function reads its actual arguments past its parameters
    /// (`arguments` or a rest parameter): entering it keeps the extra
    /// arguments above its registers.
    pub const ARGUMENTS: u16 = 1 << 4;
    /// The function reads `new.target`: its first local, right after the
    /// parameters, receives it when it is entered by a construction.
    pub const NEW_TARGET: u16 = 1 << 5;
    /// A class constructor: calling it without `new` throws.
    pub const CLASS_CONSTRUCTOR: u16 = 1 << 6;
    /// A derived class constructor: no receiver is created; `this` is bound
    /// by `super()`.
    pub const DERIVED: u16 = 1 << 7;
    /// A generator function.
    pub const GENERATOR: u16 = 1 << 8;
    /// An async function (with `GENERATOR`, an async generator).
    pub const ASYNC: u16 = 1 << 9;
    /// `MakeClosure` gives the function a `prototype` object (an ordinary
    /// function or a generator; class constructors get theirs from
    /// `MakeClass`).
    pub const PROTOTYPE: u16 = 1 << 10;
    /// Module code.
    pub const MODULE: u16 = 1 << 11;
    /// A class field or static initializer that brands its receiver with its
    /// home object's private-method brand before it runs.
    pub const BRAND: u16 = 1 << 12;
    /// The root of `eval` code: `import()` in it resolves against the code
    /// that called `eval`.
    pub const EVAL: u16 = 1 << 13;
}

/// Where a closure created by `MakeClosure` gets one captured cell, read in
/// the frame that runs the `MakeClosure`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capture {
    /// A slot with no cell (a global the closure reads by name).
    None,
    /// The cell in this register of the creating frame.
    Register(u16),
    /// The creating function's own capture at this index.
    Outer(u16),
    /// The cell in this slot of the creating frame's spill scope (a local
    /// past the register window).
    Slot(u16),
}

/// [`CodeUnit::spill_register`] of a unit without a spill scope.
pub const NO_SPILL: u16 = u16::MAX;

/// Source information kept for the disassembler and error messages.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DebugInfo {
    pub name: Box<str>,
    /// The script or module the function belongs to, for `import()`.
    pub filename: Option<std::sync::Arc<str>>,
    /// Where the instructions came from, sorted by `pc`: each entry holds
    /// for its instruction and the ones after it up to the next. Empty when
    /// the producer recorded none.
    pub positions: Box<[SourcePosition]>,
    /// The one-based line and column of the function's definition, for
    /// `Function.prototype.lineNumber` and `columnNumber`.
    pub definition: Option<(u32, u32)>,
}

/// A one-based source line and column, from instruction `pc` on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourcePosition {
    pub pc: u32,
    pub line: u32,
    pub column: u32,
}

impl DebugInfo {
    /// The line and column of the instruction at `pc`, as QuickJS's
    /// `find_line_num` reads its table: the last entry at or before it.
    pub fn position(&self, pc: u32) -> Option<(u32, u32)> {
        let after = self.positions.partition_point(|entry| entry.pc <= pc);
        let entry = self.positions[..after].last()?;
        Some((entry.line, entry.column))
    }
}

/// The compiled form of one function.
///
/// The register window of a frame is laid out as: register 0 is `this`, then
/// `parameter_count` parameters, then `local_count` locals, then the operand
/// temporaries up to `register_count`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeUnit {
    pub code: Box<[Insn]>,
    pub register_count: u16,
    pub parameter_count: u16,
    pub local_count: u16,
    pub flags: u16,
    /// Distinct `this.x =` names in a constructor body: the slots `New`
    /// reserves on the receiver.
    pub this_slots: u16,
    /// The length of the constant table attached on the heap.
    pub constant_count: u32,
    /// The register holding the frame's spill scope, or [`NO_SPILL`]: a
    /// function with more locals than the register window holds keeps the
    /// rest in the slots of one scope object, created by `NewSpill`.
    pub spill_register: u16,
    /// The slots of the spill scope.
    pub spill_slots: u16,
    /// The unit's first entry in the cache store, set when it is published.
    pub ic_base: u32,
    /// Cache sites, numbered by [`CodeBuilder::finish`](builder::CodeBuilder::finish).
    pub ic_count: u32,
    /// Inner functions, by `MakeClosure` index. Published before the unit.
    pub children: Box<[CodeUnitId]>,
    /// The function's `name` (an atom), or `None` for `""`.
    pub name_atom: Option<crate::value::StringId>,
    /// The function's `length`.
    pub length: u16,
    /// The atoms a dynamic unit keeps alive ([`CodeUnit::key_atoms`]), set
    /// when it is published; empty for a static unit, whose atoms are
    /// pinned.
    pub atoms: Box<[crate::value::StringId]>,
    /// The tables that name nothing of a runtime, shared by every runtime
    /// that loaded the unit from one code image ([`crate::image`]); the
    /// unit reads them as its own fields ([`CodeUnit`] derefs to them).
    pub body: std::sync::Arc<UnitBody>,
}

/// The parts of a unit that name nothing of a runtime: held once for every
/// runtime that runs the unit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnitBody {
    pub handlers: Box<[Handler]>,
    /// The cells a closure over this unit captures, from the frame that
    /// creates it; `LoadCaptured` indexes them.
    pub captures: Box<[Capture]>,
    pub debug: DebugInfo,
    /// The function's source text, for `Function.prototype.toString`, in
    /// WTF-8 (a lone surrogate of `Function` constructor source is kept as
    /// its three-byte form, as QuickJS's `JS_ToCStringLen2` keeps it).
    pub source: Option<Box<[u8]>>,
    /// Sorted indices of the calls written as a direct `eval`, which runs as
    /// indirect eval: a backtrace lists no native frame for them, as QuickJS's
    /// direct eval has none.
    pub direct_eval_sites: Box<[u32]>,
}

impl UnitBody {
    /// Bytes the tables hold.
    pub fn bytes(&self) -> usize {
        size_of::<UnitBody>()
            + std::mem::size_of_val(&*self.handlers)
            + std::mem::size_of_val(&*self.captures)
            + std::mem::size_of_val(&*self.debug.positions)
            + self.debug.name.len()
            + self.source.as_ref().map_or(0, |source| source.len())
            + std::mem::size_of_val(&*self.direct_eval_sites)
    }
}

impl std::ops::Deref for CodeUnit {
    type Target = UnitBody;

    #[inline(always)]
    fn deref(&self) -> &UnitBody {
        &self.body
    }
}

impl CodeUnit {
    /// The body to fill in while the unit is built (unshared then).
    pub fn body_mut(&mut self) -> &mut UnitBody {
        std::sync::Arc::make_mut(&mut self.body)
    }

    /// Bytes the unit holds itself, without its body or its cache entries.
    pub fn own_bytes(&self) -> usize {
        size_of::<CodeUnit>()
            + std::mem::size_of_val(&*self.code)
            + std::mem::size_of_val(&*self.children)
            + std::mem::size_of_val(&*self.atoms)
    }
}

impl CodeUnit {
    /// The first operand temporary, `T(0)`: after the locals, or after the
    /// spill register when the locals do not all fit in the window.
    #[inline]
    pub const fn temp_base(&self) -> u32 {
        if self.spill_register != NO_SPILL {
            self.spill_register as u32 + 1
        } else {
            1 + self.parameter_count as u32 + self.local_count as u32
        }
    }

    #[inline]
    pub const fn has(&self, flag: u16) -> bool {
        self.flags & flag != 0
    }
}

/// Marks a [`CodeUnitId`] of a unit compiled at run time, which the heap
/// owns.
pub(crate) const DYNAMIC: u32 = 1 << 31;

impl CodeUnitId {
    /// Whether the unit was compiled at run time: the heap owns it and
    /// frees it once no bytecode record names it.
    #[inline]
    pub const fn is_dynamic(self) -> bool {
        self.0 & DYNAMIC != 0
    }
}

/// A borrowed code unit: a unit of the [`CodeStore`] for the store's
/// lifetime, or a counted reference to a unit the heap owns, which keeps it
/// alive while the loop runs it across a collection.
pub enum UnitRef<'a> {
    Static(&'a CodeUnit),
    Dynamic(std::rc::Rc<CodeUnit>),
}

impl std::ops::Deref for UnitRef<'_> {
    type Target = CodeUnit;

    #[inline(always)]
    fn deref(&self) -> &CodeUnit {
        match self {
            UnitRef::Static(unit) => unit,
            UnitRef::Dynamic(unit) => unit,
        }
    }
}

/// The units that live as long as the runtime: the program's, its modules',
/// and what the embedder compiles without the `transient` option, before
/// the program runs or inside a host function. It lives beside the heap, not
/// in it, so the main loop can hold `&[Insn]` across heap calls; units
/// compiled at run time by `eval` live in the heap
/// ([`DynamicUnits`](crate::heap::code::DynamicUnits)).
///
/// The store only grows, and a unit never moves while anything can borrow
/// it. Publishing takes `&self`, so it is safe while the running loop
/// borrows a unit: a new unit goes in the overflow, chunks of cells that are
/// filled once (chunk `k` holds `FIRST_CHUNK << k`). The loop reads the
/// settled units, a plain vector; [`CodeStore::settle`] moves the overflow
/// into it whenever the store is not shared (between entries), keeping
/// every id.
#[derive(Default)]
pub struct CodeStore {
    units: Vec<CodeUnit>,
    overflow: Overflow,
}

/// Units published while the store was shared, in id order after the
/// settled ones: `CHUNKS` chunks of `CHUNK` cells, each filled once.
struct Overflow {
    chunks: [std::cell::OnceCell<Box<[std::cell::OnceCell<CodeUnit>]>>; CHUNKS],
    len: std::cell::Cell<u32>,
}

/// Units per overflow chunk.
const CHUNK: usize = 128;
/// Overflow chunks: the units a runtime can publish while its store is
/// shared before it returns to the top level; past them a host function's
/// code is transient.
const CHUNKS: usize = 256;

impl Default for Overflow {
    fn default() -> Self {
        Overflow {
            chunks: std::array::from_fn(|_| std::cell::OnceCell::new()),
            len: std::cell::Cell::new(0),
        }
    }
}

impl Overflow {
    #[inline]
    fn get(&self, index: usize) -> Option<&CodeUnit> {
        self.chunks
            .get(index / CHUNK)?
            .get()?
            .get(index % CHUNK)?
            .get()
    }
}

impl CodeStore {
    /// Validates `unit` and appends it. Its cache entries are the caller's:
    /// [`Runtime::publish`](crate::Runtime::publish) reserves them.
    pub fn publish(&self, unit: CodeUnit) -> Result<CodeUnitId, ValidateError> {
        validate::validate(&unit, &|id| self.lookup(id))?;
        match self.push(unit) {
            Ok(id) => Ok(id),
            Err(_) => panic!("code store overflow"),
        }
    }

    /// Appends a validated unit, to the overflow; gives it back when the
    /// overflow is full.
    pub(crate) fn push(&self, unit: CodeUnit) -> Result<CodeUnitId, CodeUnit> {
        if !self.has_room() {
            return Err(unit);
        }
        let index = self.overflow.len.get() as usize;
        let id = (self.units.len() + index) as u32;
        let cells = self.overflow.chunks[index / CHUNK]
            .get_or_init(|| (0..CHUNK).map(|_| std::cell::OnceCell::new()).collect());
        if cells[index % CHUNK].set(unit).is_err() {
            unreachable!("an overflow cell is filled once");
        }
        self.overflow.len.set(index as u32 + 1);
        Ok(CodeUnitId(id))
    }

    /// Whether [`CodeStore::push`] has room.
    pub(crate) fn has_room(&self) -> bool {
        let overflow = self.overflow.len.get() as usize;
        overflow < CHUNK * CHUNKS && self.units.len() + overflow < DYNAMIC as usize
    }

    /// Moves the overflow into the settled units, where the loop reads them
    /// directly. Ids do not change.
    #[inline]
    pub fn settle(&mut self) {
        if self.overflow.len.get() != 0 {
            self.settle_overflow();
        }
    }

    #[cold]
    #[inline(never)]
    fn settle_overflow(&mut self) {
        let overflow = std::mem::take(&mut self.overflow);
        let count = overflow.len.get() as usize;
        self.units.reserve(count);
        for chunk in overflow.chunks {
            let Some(cells) = chunk.into_inner() else {
                break;
            };
            for cell in cells.into_vec() {
                match cell.into_inner() {
                    Some(unit) => self.units.push(unit),
                    None => return,
                }
            }
        }
    }

    /// The unit `id`, if it is a unit of this store.
    #[inline]
    pub fn lookup(&self, id: CodeUnitId) -> Option<&CodeUnit> {
        if id.is_dynamic() {
            return None;
        }
        match self.units.get(id.0 as usize) {
            Some(unit) => Some(unit),
            None => self.overflow.get(id.0 as usize - self.units.len()),
        }
    }

    /// The static unit `id`.
    #[inline]
    pub fn get(&self, id: CodeUnitId) -> &CodeUnit {
        debug_assert!(!id.is_dynamic(), "a unit of the code store");
        let index = id.0 as usize;
        if index >= self.units.len() {
            return self.get_overflow(id);
        }
        &self.units[index]
    }

    #[cold]
    #[inline(never)]
    fn get_overflow(&self, id: CodeUnitId) -> &CodeUnit {
        self.lookup(id).expect("a published unit")
    }

    pub fn len(&self) -> usize {
        self.units.len() + self.overflow.len.get() as usize
    }

    /// Every unit with its id, in publication order.
    pub fn units(&self) -> impl Iterator<Item = (CodeUnitId, &CodeUnit)> {
        (0..self.len() as u32).map(|index| (CodeUnitId(index), self.get(CodeUnitId(index))))
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `id` and its descendants in pre-order ([`Units::tree`]).
    pub fn tree(&self, id: CodeUnitId) -> Vec<CodeUnitId> {
        tree(id, |id| &self.get(id).children[..])
    }
}

/// `id` and its descendants in pre-order, as `children` gives them.
fn tree<'a>(id: CodeUnitId, children: impl Fn(CodeUnitId) -> &'a [CodeUnitId]) -> Vec<CodeUnitId> {
    let mut order = Vec::new();
    let mut pending = vec![id];
    while let Some(id) = pending.pop() {
        order.push(id);
        pending.extend(children(id).iter().rev());
    }
    order
}

/// Every unit the runtime holds, static and dynamic, for reading.
#[derive(Clone, Copy)]
pub struct Units<'a> {
    pub(crate) code: &'a CodeStore,
    pub(crate) dynamic: &'a crate::heap::code::DynamicUnits,
}

impl<'a> Units<'a> {
    /// The unit `id`.
    #[inline]
    pub fn get(self, id: CodeUnitId) -> UnitRef<'a> {
        if id.is_dynamic() {
            return UnitRef::Dynamic(self.dynamic.get(id));
        }
        UnitRef::Static(self.code.get(id))
    }

    /// The unit `id`, if it is published.
    pub fn lookup(self, id: CodeUnitId) -> Option<&'a CodeUnit> {
        if id.is_dynamic() {
            return self.dynamic.lookup(id);
        }
        self.code.lookup(id)
    }

    /// `id` and its descendants in pre-order: the order in which
    /// [`Runtime::instantiate`](crate::Runtime::instantiate) takes their
    /// constant tables.
    pub fn tree(self, id: CodeUnitId) -> Vec<CodeUnitId> {
        tree(id, |id| {
            &self.lookup(id).expect("a published unit").children[..]
        })
    }

    /// Every unit with its id: the static ones in publication order, then
    /// the live dynamic ones by slot.
    pub fn all(self) -> impl Iterator<Item = (CodeUnitId, &'a CodeUnit)> {
        self.code.units().chain(self.dynamic.units())
    }
}

impl CodeUnit {
    /// The distinct atoms the instructions name as property keys, with the
    /// function's name: what a live dynamic unit keeps alive.
    pub(crate) fn key_atoms(&self) -> Box<[crate::value::StringId]> {
        let mut atoms: Vec<crate::value::StringId> = self.key_atom_uses().collect();
        atoms.sort_unstable();
        atoms.dedup();
        atoms.into()
    }

    /// Every atom the instructions name as a property key, and the
    /// function's name, once per use.
    pub(crate) fn key_atom_uses(&self) -> impl Iterator<Item = crate::value::StringId> + '_ {
        use crate::heap::atom::KeyKind;
        self.code
            .iter()
            .filter(|insn| matches!(insn.op.format(), Format::Key | Format::AKey | Format::ABKey))
            .filter_map(|insn| match insn.key().kind() {
                KeyKind::String(id) => Some(id),
                _ => None,
            })
            .chain(self.name_atom)
    }

    /// Bytes the unit holds: its instructions, tables, positions, source
    /// text and cache entries. A dynamic unit charges them to the heap.
    pub fn bytes(&self) -> usize {
        self.own_bytes() + self.body.bytes() + self.ic_count as usize * crate::vm::ic::ENTRY_BYTES
    }
}
