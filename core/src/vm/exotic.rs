//! Host exotic objects: objects whose internal methods are the embedder's
//! Rust hooks, as QuickJS's `JSClassExoticMethods` are. A class registers
//! its hooks once ([`NativeCx::register_exotic_class`]); each object of it is
//! an [`ObjectKind::HostExotic`] whose internal record holds the class and
//! the embedder's datum.
//!
//! Each hook answers or declines: `Ok(None)` falls back to the ordinary
//! internal method on the object (its own ordinary properties, then its
//! prototype), as QuickJS falls back when a class leaves a method `NULL`.
//! [[Get]], [[Set]] and [[HasProperty]] that decline consult
//! [[GetOwnProperty]], so a class that answers only that one behaves as a
//! complete exotic object. Property walks that meet the kind leave the
//! heap-only paths ([`ObjectKind::is_exotic`]), so no inline cache fills for
//! it or through it.
//!
//! A hook runs as a host function does: inside the loop's entry, with a
//! [`NativeCx`], counted against the native-stack budget, and with the root
//! stack cut back when it returns. The object, the key and the values it
//! receives are rooted for its duration; what it returns is unrooted.
//!
//! [`ObjectKind::HostExotic`]: crate::ObjectKind::HostExotic
//! [`ObjectKind::is_exotic`]: crate::ObjectKind::is_exotic

use std::any::Any;
use std::rc::Rc;

use super::object::Descriptor;
use super::{Cx, Throw};
use crate::embed::{NativeCx, PropertyDescriptor};
use crate::heap::atom::PropertyKey;
use crate::heap::object::InternalData;
use crate::value::{ObjectId, Value};

/// A host class's internal methods. Every method has a default that
/// declines (`Ok(None)`), so a class implements only the ones it needs.
/// `object` is the exotic object; values passed in are rooted while the
/// hook runs.
pub trait ExoticHooks {
    /// `[[GetOwnProperty]]`: `Some(None)` when `key` is absent.
    fn get_own_property(
        &self,
        cx: &mut NativeCx<'_>,
        object: ObjectId,
        key: PropertyKey,
    ) -> Result<Option<Option<PropertyDescriptor>>, Throw> {
        let _ = (cx, object, key);
        Ok(None)
    }

    /// `[[DefineOwnProperty]]`.
    fn define_own_property(
        &self,
        cx: &mut NativeCx<'_>,
        object: ObjectId,
        key: PropertyKey,
        descriptor: &PropertyDescriptor,
    ) -> Result<Option<bool>, Throw> {
        let _ = (cx, object, key, descriptor);
        Ok(None)
    }

    /// `[[HasProperty]]`, for the whole chain from `object`.
    fn has(
        &self,
        cx: &mut NativeCx<'_>,
        object: ObjectId,
        key: PropertyKey,
    ) -> Result<Option<bool>, Throw> {
        let _ = (cx, object, key);
        Ok(None)
    }

    /// `[[Get]]`, for the whole chain from `object`.
    fn get(
        &self,
        cx: &mut NativeCx<'_>,
        object: ObjectId,
        key: PropertyKey,
        receiver: Value,
    ) -> Result<Option<Value>, Throw> {
        let _ = (cx, object, key, receiver);
        Ok(None)
    }

    /// `[[Set]]`, for the whole chain from `object`; `receiver` is the
    /// object the assignment named, which is not `object` when `object` is
    /// on its prototype chain.
    fn set(
        &self,
        cx: &mut NativeCx<'_>,
        object: ObjectId,
        key: PropertyKey,
        value: Value,
        receiver: Value,
    ) -> Result<Option<bool>, Throw> {
        let _ = (cx, object, key, value, receiver);
        Ok(None)
    }

    /// `[[Delete]]`.
    fn delete(
        &self,
        cx: &mut NativeCx<'_>,
        object: ObjectId,
        key: PropertyKey,
    ) -> Result<Option<bool>, Throw> {
        let _ = (cx, object, key);
        Ok(None)
    }

    /// `[[OwnPropertyKeys]]`: the object's ordinary own keys that are not
    /// in the list follow it. The keys need not stay rooted after the hook
    /// returns: the caller roots them before anything allocates.
    fn own_keys(
        &self,
        cx: &mut NativeCx<'_>,
        object: ObjectId,
    ) -> Result<Option<Vec<PropertyKey>>, Throw> {
        let _ = (cx, object);
        Ok(None)
    }
}

/// A registered host class: an index into the runtime's class table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExoticClass(pub(crate) u32);

/// The internal datum of a host exotic object: its class and the
/// embedder's value.
#[derive(Clone)]
pub struct HostExotic {
    pub(crate) class: ExoticClass,
    pub(crate) data: Rc<dyn Any>,
}

impl std::fmt::Debug for HostExotic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostExotic")
            .field("class", &self.class)
            .finish_non_exhaustive()
    }
}

/// The hooks of `object`'s class, cloned out so the heap is free while they
/// run.
fn hooks(cx: &Cx<'_>, object: ObjectId) -> Rc<dyn ExoticHooks> {
    let InternalData::HostExotic(exotic) = &cx.heap.internal(object).data else {
        unreachable!("a host exotic object holds its class");
    };
    Rc::clone(&cx.host.exotic_classes[exotic.class.0 as usize])
}

/// Runs a hook of `object`'s class with `rooted` on the root stack, as a
/// host function runs: a re-entry against the native-stack budget, the root
/// stack cut back after.
#[inline(never)]
fn invoke<T>(
    cx: &mut Cx<'_>,
    object: ObjectId,
    key: Option<PropertyKey>,
    rooted: &[Value],
    hook: impl FnOnce(&dyn ExoticHooks, &mut NativeCx<'_>) -> Result<T, Throw>,
) -> Result<T, Throw> {
    let hooks = hooks(cx, object);
    if !cx.heap.vm.enter_native() {
        return Err(super::error::stack_overflow(cx));
    }
    let mark = cx.heap.roots().mark();
    cx.heap.roots().push(Value::object(object));
    if let Some(value) = key.and_then(PropertyKey::heap_value) {
        cx.heap.roots().push(value);
    }
    for &value in rooted {
        cx.heap.roots().push(value);
    }
    let result = hook(&*hooks, &mut NativeCx::from_cx(cx));
    cx.heap.roots().truncate(mark);
    cx.heap.vm.leave_native();
    result
}

pub(crate) fn get_own_property(
    cx: &mut Cx<'_>,
    object: ObjectId,
    key: PropertyKey,
) -> Result<Option<Option<Descriptor>>, Throw> {
    let answer = invoke(cx, object, Some(key), &[], |hooks, ncx| {
        hooks.get_own_property(ncx, object, key)
    })?;
    Ok(answer.map(|own| own.map(PropertyDescriptor::to_internal)))
}

pub(crate) fn define_own_property(
    cx: &mut Cx<'_>,
    object: ObjectId,
    key: PropertyKey,
    descriptor: &Descriptor,
) -> Result<Option<bool>, Throw> {
    let descriptor = PropertyDescriptor::from_internal(*descriptor);
    let rooted = [
        descriptor.value.unwrap_or(Value::UNDEFINED),
        descriptor.get.unwrap_or(Value::UNDEFINED),
        descriptor.set.unwrap_or(Value::UNDEFINED),
    ];
    invoke(cx, object, Some(key), &rooted, |hooks, ncx| {
        hooks.define_own_property(ncx, object, key, &descriptor)
    })
}

pub(crate) fn has(
    cx: &mut Cx<'_>,
    object: ObjectId,
    key: PropertyKey,
) -> Result<Option<bool>, Throw> {
    invoke(cx, object, Some(key), &[], |hooks, ncx| {
        hooks.has(ncx, object, key)
    })
}

pub(crate) fn get(
    cx: &mut Cx<'_>,
    object: ObjectId,
    key: PropertyKey,
    receiver: Value,
) -> Result<Option<Value>, Throw> {
    invoke(cx, object, Some(key), &[receiver], |hooks, ncx| {
        hooks.get(ncx, object, key, receiver)
    })
}

pub(crate) fn set(
    cx: &mut Cx<'_>,
    object: ObjectId,
    key: PropertyKey,
    value: Value,
    receiver: Value,
) -> Result<Option<bool>, Throw> {
    invoke(cx, object, Some(key), &[value, receiver], |hooks, ncx| {
        hooks.set(ncx, object, key, value, receiver)
    })
}

pub(crate) fn delete(
    cx: &mut Cx<'_>,
    object: ObjectId,
    key: PropertyKey,
) -> Result<Option<bool>, Throw> {
    invoke(cx, object, Some(key), &[], |hooks, ncx| {
        hooks.delete(ncx, object, key)
    })
}

pub(crate) fn own_keys(
    cx: &mut Cx<'_>,
    object: ObjectId,
) -> Result<Option<Vec<PropertyKey>>, Throw> {
    invoke(cx, object, None, &[], |hooks, ncx| {
        hooks.own_keys(ncx, object)
    })
}
