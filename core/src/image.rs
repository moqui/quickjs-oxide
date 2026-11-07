//! Code images: a compiled script's units with their constants by value, so
//! any runtime of the same build loads the script without its front end.
//!
//! An image names nothing of the runtime it came from. Atoms (the keys the
//! instructions name, the functions' names, string constants) are held by
//! text in one table; numbers by their bits; BigInts by sign and limbs;
//! template objects by their strings. A unit's instructions keep their
//! shape: each instruction whose `imm` is a string key holds the atom's
//! index in the table instead, and the unit lists those instructions, so a
//! load copies the instructions and patches the listed `imm`s with the
//! loading runtime's atoms, without decoding anything.
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
pub(crate) struct ImageUnit {
    /// The instructions; the `imm` of each one `key_sites` lists is an
    /// index into the atom table.
    pub(crate) code: Box<[Insn]>,
    pub(crate) key_sites: Box<[u32]>,
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

/// A constant by value.
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
                    + std::mem::size_of_val(&*unit.code)
                    + std::mem::size_of_val(&*unit.key_sites)
                    + std::mem::size_of_val(&*unit.children)
                    + std::mem::size_of_val(&*unit.constants)
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

/// Encodes the script whose function object is `function`.
pub(crate) fn encode(
    heap: &Heap,
    units: crate::Units<'_>,
    realm: &Realm,
    function: ObjectId,
) -> Result<CodeImage, EncodeError> {
    let root = heap
        .function_bytecode(function)
        .ok_or(EncodeError::NotBytecode)?;
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
        let mut code = unit.code.to_vec();
        let mut key_sites = Vec::new();
        for (pc, insn) in code.iter_mut().enumerate() {
            if names_key(insn)
                && let KeyKind::String(atom) = insn.key().kind()
            {
                insn.imm = self.atom(atom);
                key_sites.push(pc as u32);
            }
        }
        let name_atom = unit.name_atom.map(|atom| self.atom(atom));
        let constants = self.heap.bytecode_constants(bytecode).to_vec();
        let constants = constants
            .into_iter()
            .map(|value| self.constant(value))
            .collect::<Result<Box<[_]>, _>>()?;
        let counts = counts_of(&unit);
        let body = Arc::clone(&unit.body);
        let child_count = unit.children.len();
        drop(unit);
        let mut children = Vec::with_capacity(child_count);
        for child in 0..child_count {
            let child = self.heap.bytecode_child(bytecode, child as u32);
            children.push(self.unit(child)?);
        }
        self.out[index as usize] = Some(ImageUnit {
            code: code.into(),
            key_sites: key_sites.into(),
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

    /// The code unit of image unit `index` for this runtime, its children
    /// already published as `children`.
    pub(crate) fn unit(
        &mut self,
        heap: &mut Heap,
        index: usize,
        children: Box<[CodeUnitId]>,
    ) -> Result<CodeUnit, LoadError> {
        let image = self.image;
        let unit = &image.units[index];
        let mut code = unit.code.to_vec();
        for &pc in &unit.key_sites {
            let insn = code
                .get_mut(pc as usize)
                .filter(|insn| names_key(insn))
                .ok_or(LoadError::Malformed("a key site"))?;
            let atom = self.atom(heap, insn.imm)?;
            insn.imm = crate::PropertyKey::atom(atom).raw();
        }
        let name_atom = unit
            .name_atom
            .map(|index| self.atom(heap, index))
            .transpose()?;
        let counts = unit.counts;
        Ok(CodeUnit {
            code: code.into(),
            register_count: counts.register_count,
            parameter_count: counts.parameter_count,
            local_count: counts.local_count,
            flags: counts.flags,
            this_slots: counts.this_slots,
            constant_count: counts.constant_count,
            spill_register: counts.spill_register,
            spill_slots: counts.spill_slots,
            ic_base: 0,
            ic_count: counts.ic_count,
            children,
            name_atom,
            length: counts.length,
            atoms: Box::new([]),
            body: Arc::clone(&unit.body),
        })
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
