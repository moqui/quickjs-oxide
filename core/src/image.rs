//! Code images: a compiled script's units with their constants by value, so
//! any runtime of the same build loads the script without its front end.
//!
//! An image names nothing of the runtime it came from. Atoms (the keys the
//! instructions name, the functions' names, string constants) are held by
//! text in one table; numbers by their bits; BigInts by sign and limbs;
//! template objects by their strings. A unit's instructions name nothing of
//! a runtime (a key operand indexes the unit's key table), so they are part
//! of the unit's body, which the image and every runtime that loads it
//! share: a load builds each unit's key table from the image's (interning
//! the atoms) and copies no instruction.
//!
//! [`Compiler::load_image`](crate::vm::compile::Compiler::load_image)
//! publishes every unit through the validator (a malformed image fails
//! there, never at run time), with fresh inline caches, and instantiates the
//! root as [`Runtime::instantiate`](crate::Runtime::instantiate) does. An
//! image is `Send + Sync` and cheap to clone: a process keeps one per script
//! and every thread's runtime loads it.

use std::sync::Arc;

use crate::bytecode::{CodeUnit, CodeUnitId, Format, Insn, UnitBody};
use crate::heap::Heap;
use crate::heap::atom::KeyKind;
use crate::heap::string::StringRepr;
use crate::runtime::{Names, Realm};
use crate::value::{BytecodeId, Kind, ObjectId, Value};

/// A script's code by value. Cloning shares it.
#[derive(Clone)]
pub struct CodeImage(pub(crate) Arc<ImageData>);

impl std::fmt::Debug for CodeImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeImage")
            .field("units", &self.0.units.len())
            .field("atoms", &self.0.atoms.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub(crate) struct ImageData {
    /// The atom table: every atom the units name, by text.
    pub(crate) atoms: Box<[StringRepr]>,
    /// The units in the pre-order of [`Units::tree`](crate::Units::tree):
    /// the root first.
    pub(crate) units: Box<[ImageUnit]>,
}

/// One unit: its instructions with their atoms by index, its counts, its
/// children by index, its constant table, and its body, which every runtime
/// that loads the image shares.
#[derive(Clone)]
pub(crate) struct ImageUnit {
    /// The unit's key table: what each key operand of its instructions
    /// (which are in the shared body) names.
    pub(crate) keys: Box<[ImageKey]>,
    /// The instructions, shared with every runtime that loads the image.
    pub(crate) code: Arc<[Insn]>,
    pub(crate) counts: Counts,
    pub(crate) body: Arc<UnitBody>,
    /// Indices into the image's units, in `MakeClosure` order.
    pub(crate) children: Box<[u32]>,
    pub(crate) name_atom: Option<u32>,
    pub(crate) constants: Box<[Constant]>,
}

/// A unit's counts and flags, which the code store keeps beside its
/// instructions.
#[derive(Clone, Copy)]
pub(crate) struct Counts {
    pub(crate) register_count: u16,
    pub(crate) parameter_count: u16,
    pub(crate) local_count: u16,
    pub(crate) flags: u16,
    pub(crate) this_slots: u16,
    pub(crate) constant_count: u32,
    pub(crate) spill_register: u16,
    pub(crate) spill_slots: u16,
    pub(crate) ic_count: u32,
    pub(crate) length: u16,
}

/// A key of a unit's key table.
#[derive(Clone, Copy)]
pub(crate) enum ImageKey {
    /// An atom of the table.
    Atom(u32),
    /// An array index, by its key's bits.
    Index(u32),
}

/// A constant by value.
#[derive(Clone)]
pub(crate) enum Constant {
    /// A number, a boolean, `undefined` or `null`: the value's bits.
    Bits(u64),
    /// An atom of the table.
    Atom(u32),
    /// A string that is not an atom (a literal spelling an index).
    String(StringRepr),
    BigInt {
        negative: bool,
        limbs: Box<[u64]>,
    },
    /// A tagged template's site object: its cooked strings (`None` for
    /// `undefined`) and raw strings, as atoms.
    Template {
        cooked: Box<[Option<u32>]>,
        raw: Box<[u32]>,
    },
}

/// Why an image did not load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// An index or a count the image holds is out of range.
    Malformed(&'static str),
    /// A unit failed the publication validator.
    Invalid(crate::bytecode::ValidateError),
    /// The heap could not hold the script.
    Heap(crate::HeapError),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Malformed(what) => write!(f, "malformed code image: {what}"),
            LoadError::Invalid(error) => write!(f, "invalid code image: {error}"),
            LoadError::Heap(error) => write!(f, "{error:?}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<crate::HeapError> for LoadError {
    fn from(error: crate::HeapError) -> Self {
        LoadError::Heap(error)
    }
}

/// Why a value could not be encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// The value is not a bytecode function.
    NotBytecode,
    /// A constant of a kind an image does not hold.
    Constant(&'static str),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::NotBytecode => f.write_str("not a compiled script"),
            EncodeError::Constant(kind) => write!(f, "a constant of kind {kind}"),
        }
    }
}

impl std::error::Error for EncodeError {}

impl CodeImage {
    /// A copy of the image with one part broken, for the loader's tests:
    /// `key-operand`, `key-kind`,
    /// `atom-index`, `child-index`, `child-twice`, `constant-count`,
    /// `constant-bits`, `ic-count`, `root-captures`, `root-flags`,
    /// `yield-flag` or `register`. `None` when the image has nothing of that
    /// part to break.
    #[doc(hidden)]
    pub fn corrupted(&self, which: &str) -> Option<CodeImage> {
        let mut data = (*self.0).clone();
        let atoms = data.atoms.len() as u32;
        let units = &mut data.units;
        let with_keys = units.iter().position(|unit| !unit.keys.is_empty());
        let with_children = units.iter().position(|unit| !unit.children.is_empty());
        match which {
            "key-operand" => {
                let unit = &mut units[with_keys?];
                let mut code = unit.code.to_vec();
                let insn = code.iter_mut().find(|insn| names_key(insn))?;
                insn.imm = unit.keys.len() as u32;
                unit.code = code.into();
            }
            "key-kind" => {
                let unit = &mut units[with_keys?];
                let mut keys = unit.keys.to_vec();
                keys[0] = ImageKey::Index(0);
                unit.keys = keys.into();
            }
            "atom-index" => {
                let unit = &mut units[with_keys?];
                let mut keys = unit.keys.to_vec();
                keys[0] = ImageKey::Atom(atoms);
                unit.keys = keys.into();
            }
            "child-index" => {
                let unit = &mut units[with_children?];
                let mut children = unit.children.to_vec();
                children[0] = 0;
                unit.children = children.into();
            }
            "child-twice" => {
                let unit = &mut units[with_children?];
                let mut children = unit.children.to_vec();
                children.push(children[0]);
                unit.children = children.into();
            }
            "constant-count" => units[0].counts.constant_count += 1,
            "constant-bits" => {
                let unit = units.iter_mut().find(|unit| !unit.constants.is_empty())?;
                let mut constants = unit.constants.to_vec();
                constants[0] = Constant::Bits(Value::object(ObjectId(0)).bits());
                unit.constants = constants.into();
            }
            "ic-count" => units[0].counts.ic_count = u32::MAX,
            "root-captures" => {
                let mut body = (*units[0].body).clone();
                body.captures = vec![crate::bytecode::Capture::Register(0)].into();
                units[0].body = Arc::new(body);
            }
            "root-flags" => units[0].counts.flags |= crate::bytecode::flags::GENERATOR,
            "yield-flag" => {
                let unit = units
                    .iter_mut()
                    .find(|unit| unit.counts.flags & crate::bytecode::flags::GENERATOR != 0)?;
                unit.counts.flags &= !crate::bytecode::flags::GENERATOR;
            }
            "register" => {
                let unit = units.iter_mut().find(|unit| {
                    unit.code
                        .iter()
                        .any(|insn| insn.op.format() == crate::bytecode::Format::AB)
                })?;
                let mut code = unit.code.to_vec();
                let insn = code
                    .iter_mut()
                    .find(|insn| insn.op.format() == crate::bytecode::Format::AB)?;
                insn.b = 255;
                unit.code = code.into();
            }
            _ => return None,
        }
        Some(CodeImage(Arc::new(data)))
    }

    /// The number of units.
    pub fn unit_count(&self) -> usize {
        self.0.units.len()
    }

    /// Bytes the image holds, approximately: instructions, tables, source
    /// text, positions, atoms and constants.
    pub fn bytes(&self) -> usize {
        let atoms: usize = self
            .0
            .atoms
            .iter()
            .map(|atom| match atom {
                StringRepr::OneByte(bytes) => bytes.len(),
                StringRepr::TwoByte(units) => units.len() * 2,
            })
            .sum();
        let units: usize = self
            .0
            .units
            .iter()
            .map(|unit| {
                std::mem::size_of::<ImageUnit>()
                    + std::mem::size_of_val(&*unit.keys)
                    + std::mem::size_of_val(&*unit.children)
                    + std::mem::size_of_val(&*unit.constants)
                    + std::mem::size_of_val(&*unit.code)
                    + unit.body.bytes()
            })
            .sum();
        atoms + units
    }
}

/// Whether `insn`'s `imm` is a property key.
fn names_key(insn: &Insn) -> bool {
    matches!(insn.op.format(), Format::Key | Format::AKey | Format::ABKey)
}

/// Encodes the script whose function object is `function`: a script's
/// root, which captures no cell (its captures are the globals it names). Any other value, a closure among them
/// (its captured cells are its runtime's), is [`EncodeError::NotBytecode`].
pub(crate) fn encode(
    heap: &Heap,
    units: crate::Units<'_>,
    realm: &Realm,
    function: ObjectId,
) -> Result<CodeImage, EncodeError> {
    if heap.object_kind(function) != crate::ObjectKind::Function {
        return Err(EncodeError::NotBytecode);
    }
    let root = heap
        .function_bytecode(function)
        .ok_or(EncodeError::NotBytecode)?;
    let unit = units.get(heap.bytecode_unit(root));
    let captures_nothing = unit
        .captures
        .iter()
        .all(|capture| *capture == crate::bytecode::Capture::None);
    if !unit.has(crate::bytecode::flags::SCRIPT) || !captures_nothing {
        return Err(EncodeError::NotBytecode);
    }
    drop(unit);
    let mut encoder = Encoder {
        heap,
        units,
        names: &realm.names,
        atoms: Vec::new(),
        atom_index: crate::FxHashMap::default(),
        out: Vec::new(),
    };
    encoder.unit(root)?;
    Ok(CodeImage(Arc::new(ImageData {
        atoms: encoder.atoms.into(),
        units: encoder
            .out
            .into_iter()
            .map(|unit| unit.expect("every unit encoded"))
            .collect(),
    })))
}

struct Encoder<'a> {
    heap: &'a Heap,
    units: crate::Units<'a>,
    names: &'a Names,
    atoms: Vec<StringRepr>,
    atom_index: crate::FxHashMap<crate::value::StringId, u32>,
    /// The units in pre-order; a slot is filled once its unit is encoded.
    out: Vec<Option<ImageUnit>>,
}

impl Encoder<'_> {
    fn atom(&mut self, id: crate::value::StringId) -> u32 {
        if let Some(&index) = self.atom_index.get(&id) {
            return index;
        }
        let index = self.atoms.len() as u32;
        self.atoms.push(self.heap.string(id).repr().clone());
        self.atom_index.insert(id, index);
        index
    }

    /// Encodes the unit of `bytecode` and, after it in pre-order, its
    /// children; returns its index.
    fn unit(&mut self, bytecode: BytecodeId) -> Result<u32, EncodeError> {
        let index = self.out.len() as u32;
        self.out.push(None);
        let id = self.heap.bytecode_unit(bytecode);
        let unit = self.units.get(id);
        let mut keys = Vec::with_capacity(unit.keys.len());
        for key in unit.keys.iter() {
            keys.push(match key.kind() {
                KeyKind::String(atom) => ImageKey::Atom(self.atom(atom)),
                KeyKind::Index(_) => ImageKey::Index(key.raw()),
                // A symbol is its runtime's.
                KeyKind::Symbol(_) => return Err(EncodeError::Constant("symbol key")),
            });
        }
        let name_atom = unit.name_atom.map(|atom| self.atom(atom));
        let constants = self.heap.bytecode_constants(bytecode).to_vec();
        let constants = constants
            .into_iter()
            .map(|value| self.constant(value))
            .collect::<Result<Box<[_]>, _>>()?;
        let counts = counts_of(&unit);
        let body = Arc::clone(&unit.body);
        let code = Arc::clone(&unit.code);
        let child_count = unit.children.len();
        drop(unit);
        let mut children = Vec::with_capacity(child_count);
        for child in 0..child_count {
            let child = self.heap.bytecode_child(bytecode, child as u32);
            children.push(self.unit(child)?);
        }
        self.out[index as usize] = Some(ImageUnit {
            keys: keys.into(),
            code,
            counts,
            body,
            children: children.into(),
            name_atom,
            constants,
        });
        Ok(index)
    }

    fn constant(&mut self, value: Value) -> Result<Constant, EncodeError> {
        Ok(match value.kind() {
            Kind::Number | Kind::Int32 | Kind::Bool | Kind::Undefined | Kind::Null => {
                Constant::Bits(value.bits())
            }
            Kind::String => {
                let id = value.as_string().expect("a string");
                let record = self.heap.string(id);
                if record.is_atom {
                    Constant::Atom(self.atom(id))
                } else {
                    Constant::String(record.repr().clone())
                }
            }
            Kind::BigInt => {
                let record = self.heap.bigint(value.as_bigint().expect("a BigInt"));
                Constant::BigInt {
                    negative: record.negative,
                    limbs: record.limbs.clone(),
                }
            }
            Kind::Object => self.template(value.as_object().expect("an object"))?,
            Kind::Symbol => return Err(EncodeError::Constant("symbol")),
            Kind::Empty => return Err(EncodeError::Constant("empty")),
        })
    }

    /// A template site object: a frozen array of the cooked strings whose
    /// `raw` is a frozen array of the raw ones.
    fn template(&mut self, object: ObjectId) -> Result<Constant, EncodeError> {
        let heap = self.heap;
        let not_template = EncodeError::Constant("object");
        if heap.object_kind(object) != crate::ObjectKind::Array {
            return Err(not_template);
        }
        let elements = |array: ObjectId| -> Option<Vec<Value>> {
            (0..heap.array_length(array))
                .map(|index| {
                    let key = crate::PropertyKey::index(index)?;
                    let (slot, _) = heap.lookup_own(array, key)?;
                    Some(heap.get_slot(array, slot))
                })
                .collect()
        };
        let (slot, _) = heap
            .lookup_own(object, Names::key(self.names.raw))
            .ok_or(EncodeError::Constant("object"))?;
        let raw_array = heap
            .get_slot(object, slot)
            .as_object()
            .ok_or(EncodeError::Constant("object"))?;
        let cooked = elements(object).ok_or(EncodeError::Constant("object"))?;
        let raw = elements(raw_array).ok_or(EncodeError::Constant("object"))?;
        let mut atom_of = |value: Value| -> Result<u32, EncodeError> {
            let id = value
                .as_string()
                .filter(|&id| heap.string(id).is_atom)
                .ok_or(EncodeError::Constant("object"))?;
            Ok(self.atom(id))
        };
        let cooked = cooked
            .into_iter()
            .map(|value| {
                if value.is_undefined() {
                    Ok(None)
                } else {
                    atom_of(value).map(Some)
                }
            })
            .collect::<Result<Box<[_]>, _>>()?;
        let raw = raw
            .into_iter()
            .map(&mut atom_of)
            .collect::<Result<Box<[_]>, _>>()?;
        Ok(Constant::Template { cooked, raw })
    }
}

fn counts_of(unit: &CodeUnit) -> Counts {
    Counts {
        register_count: unit.register_count,
        parameter_count: unit.parameter_count,
        local_count: unit.local_count,
        flags: unit.flags,
        this_slots: unit.this_slots,
        constant_count: unit.constant_count,
        spill_register: unit.spill_register,
        spill_slots: unit.spill_slots,
        ic_count: unit.ic_count,
        length: unit.length,
    }
}

/// Checks what the validator does not see before a load changes anything:
/// the unit tree is the image's pre-order, every atom, key and constant
/// index is in range,
/// the counts fit the code, the root is a script that captures no cell, and
/// only a generator yields and only an async function awaits.
pub(crate) fn check(image: &ImageData) -> Result<(), LoadError> {
    use crate::bytecode::{Opcode, flags};
    let malformed = LoadError::Malformed;
    let count = image.units.len();
    let atoms = image.atoms.len();
    let root = image
        .units
        .first()
        .ok_or(malformed("an image without units"))?;
    if root.counts.flags & flags::SCRIPT == 0
        || root.counts.flags & (flags::MODULE | flags::GENERATOR | flags::ASYNC) != 0
        || root
            .body
            .captures
            .iter()
            .any(|capture| *capture != crate::bytecode::Capture::None)
    {
        return Err(malformed("the root"));
    }
    // The pre-order walk from the root names every unit once, in order.
    let mut next = 0usize;
    let mut pending = vec![0u32];
    while let Some(index) = pending.pop() {
        if index as usize != next {
            return Err(malformed("the unit tree"));
        }
        next += 1;
        let unit = &image.units[index as usize];
        for &child in unit.children.iter().rev() {
            if child as usize >= count || child <= index {
                return Err(malformed("a child index"));
            }
            pending.push(child);
        }
        if next > count {
            return Err(malformed("the unit tree"));
        }
    }
    if next != count {
        return Err(malformed("the unit tree"));
    }
    for unit in image.units.iter() {
        let counts = unit.counts;
        if counts.ic_count as usize > unit.code.len() + 1
            || unit.constants.len() != counts.constant_count as usize
        {
            return Err(malformed("a unit's counts"));
        }
        for key in unit.keys.iter() {
            let valid = match *key {
                ImageKey::Atom(atom) => (atom as usize) < atoms,
                ImageKey::Index(bits) => {
                    matches!(crate::PropertyKey::from_raw(bits).kind(), KeyKind::Index(_))
                }
            };
            if !valid {
                return Err(malformed("a key"));
            }
        }
        for insn in unit.code.iter() {
            let generator = matches!(
                insn.op,
                Opcode::InitialYield | Opcode::Yield | Opcode::YieldStar
            );
            let awaits = matches!(insn.op, Opcode::Await | Opcode::AsyncStart);
            if (generator && counts.flags & flags::GENERATOR == 0)
                || (awaits && counts.flags & flags::ASYNC == 0)
            {
                return Err(malformed("an instruction the unit's flags do not allow"));
            }
        }
        if unit.name_atom.is_some_and(|atom| atom as usize >= atoms) {
            return Err(malformed("an atom index"));
        }
        for constant in unit.constants.iter() {
            let in_range = |atom: &u32| (*atom as usize) < atoms;
            let valid = match constant {
                Constant::Bits(bits) => matches!(
                    Value::from_bits(*bits).kind(),
                    Kind::Number | Kind::Int32 | Kind::Bool | Kind::Undefined | Kind::Null
                ),
                Constant::Atom(atom) => in_range(atom),
                Constant::String(_) | Constant::BigInt { .. } => true,
                Constant::Template { cooked, raw } => {
                    cooked.iter().flatten().all(in_range) && raw.iter().all(in_range)
                }
            };
            if !valid {
                return Err(malformed("a constant"));
            }
        }
    }
    Ok(())
}

/// What a load builds for one runtime: the atoms it interned.
pub(crate) struct Loader<'i> {
    pub(crate) image: &'i ImageData,
    pub(crate) atoms: Vec<Option<crate::value::StringId>>,
}

impl<'i> Loader<'i> {
    pub(crate) fn new(image: &'i CodeImage) -> Self {
        Loader {
            image: &image.0,
            atoms: vec![None; image.0.atoms.len()],
        }
    }

    pub(crate) fn atom(
        &mut self,
        heap: &mut Heap,
        index: u32,
    ) -> Result<crate::value::StringId, LoadError> {
        let slot = self
            .atoms
            .get_mut(index as usize)
            .ok_or(LoadError::Malformed("an atom index"))?;
        if let Some(id) = *slot {
            return Ok(id);
        }
        let id = match &self.image.atoms[index as usize] {
            StringRepr::OneByte(bytes) => heap.intern_latin1(bytes)?,
            StringRepr::TwoByte(units) => heap.intern_units(units)?,
        };
        *slot = Some(id);
        Ok(id)
    }

    /// The code unit of image unit `index` before it names this runtime:
    /// its instructions with the image's atom indices, its children as
    /// image indices (`CodeUnitId(child)`), for the validator.
    pub(crate) fn provisional(&self, index: usize) -> CodeUnit {
        let unit = &self.image.units[index];
        let counts = unit.counts;
        CodeUnit {
            code: Arc::clone(&unit.code),
            keys: vec![crate::PropertyKey::from_raw(0); unit.keys.len()].into(),
            register_count: counts.register_count,
            parameter_count: counts.parameter_count,
            local_count: counts.local_count,
            flags: counts.flags,
            this_slots: counts.this_slots,
            constant_count: counts.constant_count,
            spill_register: counts.spill_register,
            spill_slots: counts.spill_slots,
            ic_base: std::cell::Cell::new(crate::bytecode::IC_UNRESERVED),
            ic_count: counts.ic_count,
            children: unit
                .children
                .iter()
                .map(|&child| CodeUnitId(child))
                .collect(),
            name_atom: None,
            length: counts.length,
            atoms: Box::new([]),
            body: Arc::clone(&unit.body),
        }
    }

    /// Makes the provisional unit of image unit `index` this runtime's: its
    /// keys and name its atoms, its children their published ids.
    pub(crate) fn patch(
        &mut self,
        heap: &mut Heap,
        index: usize,
        unit: &mut CodeUnit,
        children: Box<[CodeUnitId]>,
    ) -> Result<(), LoadError> {
        let image = self.image;
        let source = &image.units[index];
        let mut keys = Vec::with_capacity(source.keys.len());
        for key in source.keys.iter() {
            keys.push(match *key {
                ImageKey::Atom(atom) => crate::PropertyKey::atom(self.atom(heap, atom)?),
                ImageKey::Index(bits) => crate::PropertyKey::from_raw(bits),
            });
        }
        unit.keys = keys.into();
        unit.name_atom = source
            .name_atom
            .map(|index| self.atom(heap, index))
            .transpose()?;
        unit.children = children;
        Ok(())
    }

    /// The constant table of image unit `index`, each value pushed on the
    /// root stack (the caller cuts it back after instantiating).
    pub(crate) fn constants(
        &mut self,
        heap: &mut Heap,
        realm: &Realm,
        index: usize,
        template: impl Fn(&mut Heap, &Realm, &[Value], &[Value]) -> Result<ObjectId, crate::HeapError>,
    ) -> Result<Vec<Value>, LoadError> {
        let image = self.image;
        let unit = &image.units[index];
        if unit.constants.len() != unit.counts.constant_count as usize {
            return Err(LoadError::Malformed("a constant table's length"));
        }
        let mut values = Vec::with_capacity(unit.constants.len());
        for constant in &unit.constants {
            let value = match constant {
                Constant::Bits(bits) => {
                    let value = Value::from_bits(*bits);
                    if !matches!(
                        value.kind(),
                        Kind::Number | Kind::Int32 | Kind::Bool | Kind::Undefined | Kind::Null
                    ) {
                        return Err(LoadError::Malformed("a constant's bits"));
                    }
                    value
                }
                Constant::Atom(atom) => Value::string(self.atom(heap, *atom)?),
                Constant::String(repr) => Value::string(heap.alloc_string_repr(repr.clone())?),
                Constant::BigInt { negative, limbs } => {
                    Value::bigint(heap.alloc_bigint(*negative, limbs)?)
                }
                Constant::Template { cooked, raw } => {
                    let mut cooked_values = Vec::with_capacity(cooked.len());
                    for atom in cooked.iter() {
                        cooked_values.push(match atom {
                            Some(atom) => Value::string(self.atom(heap, *atom)?),
                            None => Value::UNDEFINED,
                        });
                    }
                    let mut raw_values = Vec::with_capacity(raw.len());
                    for &atom in raw.iter() {
                        raw_values.push(Value::string(self.atom(heap, atom)?));
                    }
                    Value::object(template(heap, realm, &cooked_values, &raw_values)?)
                }
            };
            heap.roots().push(value);
            values.push(value);
        }
        Ok(values)
    }
}
