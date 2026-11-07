//! Host exotic objects: objects whose internal methods are an embedder's
//! Rust hooks, as QuickJS's `JSClassExoticMethods` are. A class implements
//! [`ExoticClass`] and is registered once
//! ([`Js::register_exotic_class`](super::Js::register_exotic_class)); its
//! objects ([`Js::new_host_exotic`](super::Js::new_host_exotic)) run its
//! hooks for every property operation, through an [`ExoticScope`].

use std::any::Any;
use std::marker::PhantomData;
use std::rc::{Rc, Weak};

use oxide_core::vm::exotic::ExoticHooks;
use oxide_core::{NativeCx, ObjectId, Throw};

use super::RuntimeError;
use super::function::{PoisonGuard, error_throw};
use super::ops::{JsResult, View};
use super::property::{
    AccessorValue, CompleteOrdinaryPropertyDescriptor, DescriptorField, OrdinaryPropertyDescriptor,
    PropertyKey,
};
use super::runtime::Shared;
use super::value::{CallableRef, ObjectRef, Value};

/// A hook's answer: [`Hook::Ordinary`] declines, and the engine runs the
/// ordinary internal method on the object (its own ordinary properties,
/// then its prototype), as QuickJS does for a method a class leaves `NULL`.
#[derive(Clone, Debug, PartialEq)]
pub enum Hook<T> {
    Ordinary,
    Handled(T),
}

/// A host exotic class: the internal methods of its objects. Every hook
/// defaults to [`Hook::Ordinary`], so a class implements only those it
/// needs. `data` is the object's host datum; [`ExoticScope::this`] is the
/// object.
///
/// [`ExoticClass::get`], [`ExoticClass::set`] and [`ExoticClass::has`]
/// answer for the whole prototype chain from the object; when they decline,
/// the ordinary method asks [`ExoticClass::get_own_property`] for the
/// object's own property and continues to its prototype. A class that
/// answers only `get_own_property` is therefore complete for reads; one
/// that answers `get`, `set` and `has` is the usual shape of a name
/// resolver. A `set` that declines ends in `OrdinarySet`, which defines the
/// property on the receiver through its `[[DefineOwnProperty]]`. A
/// `define_own_property` or `delete` that declines is the ordinary method
/// over `get_own_property`'s answer: a property the class reports as
/// non-configurable is neither deleted nor redefined incompatibly, and an
/// accepted definition is stored as an ordinary own property.
///
/// Hooks run inside the engine as host functions do: an error is thrown
/// as a host function's is, a panic poisons the runtime, and the engine
/// does not check the invariants a proxy's traps must keep.
#[allow(unused_variables)]
pub trait ExoticClass: 'static {
    /// The objects' host datum.
    type Data: 'static;

    /// `[[GetOwnProperty]]`: `Handled(None)` when `key` is absent.
    fn get_own_property(
        &self,
        scope: &mut ExoticScope<'_>,
        data: &Self::Data,
        key: &PropertyKey,
    ) -> JsResult<Hook<Option<CompleteOrdinaryPropertyDescriptor>>> {
        Ok(Hook::Ordinary)
    }

    /// `[[DefineOwnProperty]]`, for `Object.defineProperty` and for an
    /// assignment whose receiver is this object.
    fn define_own_property(
        &self,
        scope: &mut ExoticScope<'_>,
        data: &Self::Data,
        key: &PropertyKey,
        descriptor: &OrdinaryPropertyDescriptor,
    ) -> JsResult<Hook<bool>> {
        Ok(Hook::Ordinary)
    }

    /// `[[HasProperty]]`: `in`, and a bare name the object resolves.
    fn has(
        &self,
        scope: &mut ExoticScope<'_>,
        data: &Self::Data,
        key: &PropertyKey,
    ) -> JsResult<Hook<bool>> {
        Ok(Hook::Ordinary)
    }

    /// `[[Get]]`; [`ExoticScope::receiver`] is the value the read started
    /// from.
    fn get(
        &self,
        scope: &mut ExoticScope<'_>,
        data: &Self::Data,
        key: &PropertyKey,
    ) -> JsResult<Hook<Value>> {
        Ok(Hook::Ordinary)
    }

    /// `[[Set]]`; [`ExoticScope::receiver`] is the object the assignment
    /// named, an ordinary object when this one is on its prototype chain
    /// (the global object for a bare name when this object is its
    /// prototype). `false` is a failed assignment: a `TypeError` in strict
    /// code.
    fn set(
        &self,
        scope: &mut ExoticScope<'_>,
        data: &Self::Data,
        key: &PropertyKey,
        value: Value,
    ) -> JsResult<Hook<bool>> {
        Ok(Hook::Ordinary)
    }

    /// `[[Delete]]`.
    fn delete(
        &self,
        scope: &mut ExoticScope<'_>,
        data: &Self::Data,
        key: &PropertyKey,
    ) -> JsResult<Hook<bool>> {
        Ok(Hook::Ordinary)
    }

    /// `[[OwnPropertyKeys]]`: `Object.keys`, `for-in`, `JSON.stringify`
    /// and the like, which then read each key's enumerability through
    /// [`ExoticClass::get_own_property`]. The object's ordinary own keys
    /// are added to the list, repeats are dropped, and the result is in an
    /// ordinary object's order: integer indices ascending, then strings,
    /// then symbols, each in list order.
    fn own_keys(
        &self,
        scope: &mut ExoticScope<'_>,
        data: &Self::Data,
    ) -> JsResult<Hook<Vec<PropertyKey>>> {
        Ok(Hook::Ordinary)
    }
}

/// A registered [`ExoticClass`] of one runtime, which
/// [`Js::new_host_exotic`](super::Js::new_host_exotic) makes objects of.
/// Cloning it names the same class.
pub struct HostClass<C> {
    pub(crate) id: oxide_core::vm::exotic::ExoticClass,
    pub(crate) shared: Weak<Shared>,
    class: PhantomData<fn() -> C>,
}

impl<C> Clone for HostClass<C> {
    fn clone(&self) -> Self {
        HostClass {
            id: self.id,
            shared: Weak::clone(&self.shared),
            class: PhantomData,
        }
    }
}

impl<C> std::fmt::Debug for HostClass<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("HostClass").field(&self.id).finish()
    }
}

/// A hook's view of the engine: the object whose hook runs, the receiver
/// of a `[[Get]]` or `[[Set]]`, and every operation of [`Js`](super::Js).
/// Both are read on demand, so a hook that does not ask makes no handle.
pub struct ExoticScope<'a> {
    pub(crate) core: NativeCx<'a>,
    pub(crate) shared: Rc<Shared>,
    this: ObjectId,
    receiver: oxide_core::Value,
}

impl ExoticScope<'_> {
    /// The operations' view of the engine for one operation.
    pub(crate) fn view(&mut self) -> View<'_> {
        View::new(self.core.reborrow(), &self.shared)
    }

    /// The exotic object whose hook runs.
    pub fn this(&mut self) -> ObjectRef {
        let this = self.this;
        self.view().lift_object(this)
    }

    /// The receiver of the `[[Get]]` or `[[Set]]` that runs the hook: the
    /// value the read or the assignment named, which is not the exotic
    /// object when it is on the receiver's prototype chain or when
    /// `Reflect.get` or `Reflect.set` passed another. For the other hooks,
    /// the exotic object.
    pub fn receiver(&mut self) -> Value {
        let receiver = self.receiver;
        // Lifting fails only when a string cannot be copied out (the heap
        // cap), which reads as `undefined`, as a host function's call does.
        self.view().lift(receiver).unwrap_or(Value::Undefined)
    }

    /// Whether the receiver is the exotic object itself.
    pub fn receiver_is_this(&self) -> bool {
        self.receiver.as_object() == Some(self.this)
    }
}

/// The core's hooks for an [`ExoticClass`]: each builds the
/// [`ExoticScope`], converts the call's values, runs the class's hook and
/// lowers its answer. The runtime is held weakly, since the hooks live in
/// it.
struct Hooks<C> {
    shared: Weak<Shared>,
    class: C,
}

impl<C: ExoticClass> Hooks<C> {
    fn run<R, T>(
        &self,
        core: &mut NativeCx<'_>,
        object: ObjectId,
        receiver: oxide_core::Value,
        hook: impl FnOnce(&C, &mut ExoticScope<'_>, &C::Data) -> JsResult<Hook<R>>,
        lower: impl FnOnce(&mut View<'_>, R) -> JsResult<T>,
    ) -> Result<Option<T>, Throw> {
        let shared = self
            .shared
            .upgrade()
            .expect("an exotic object's hooks run inside its live runtime");
        let _guard = PoisonGuard(&shared);
        let data: Option<Rc<dyn Any>> = core.host_data(object);
        let result = match data
            .as_deref()
            .and_then(|data| data.downcast_ref::<C::Data>())
        {
            Some(data) => {
                let mut scope = ExoticScope {
                    core: core.reborrow(),
                    shared: Rc::clone(&shared),
                    this: object,
                    receiver,
                };
                hook(&self.class, &mut scope, data)
            }
            None => Err(RuntimeError::Invariant("an exotic object's host datum")),
        };
        let mut view = View::new(core.reborrow(), &shared);
        let completion = match result {
            Ok(Hook::Ordinary) => Ok(None),
            Ok(Hook::Handled(answer)) => lower(&mut view, answer)
                .map(Some)
                .map_err(|error| error_throw(&mut view, error)),
            Err(error) => Err(error_throw(&mut view, error)),
        };
        drop(view);
        drop(data);
        shared.drain_released(core);
        completion
    }
}

fn keep<T>(_: &mut View<'_>, answer: T) -> JsResult<T> {
    Ok(answer)
}

impl<C: ExoticClass> ExoticHooks for Hooks<C> {
    fn get_own_property(
        &self,
        core: &mut NativeCx<'_>,
        object: ObjectId,
        key: oxide_core::PropertyKey,
    ) -> Result<Option<Option<oxide_core::PropertyDescriptor>>, Throw> {
        self.run(
            core,
            object,
            oxide_core::Value::object(object),
            |class, scope, data| {
                let key = scope.view().lift_key(key)?;
                class.get_own_property(scope, data, &key)
            },
            |view, own| match own {
                Some(own) => view.lower_complete_descriptor(&own).map(Some),
                None => Ok(None),
            },
        )
    }

    fn define_own_property(
        &self,
        core: &mut NativeCx<'_>,
        object: ObjectId,
        key: oxide_core::PropertyKey,
        descriptor: &oxide_core::PropertyDescriptor,
    ) -> Result<Option<bool>, Throw> {
        self.run(
            core,
            object,
            oxide_core::Value::object(object),
            |class, scope, data| {
                let mut view = scope.view();
                let key = view.lift_key(key)?;
                let descriptor = view.lift_partial_descriptor(descriptor)?;
                drop(view);
                class.define_own_property(scope, data, &key, &descriptor)
            },
            keep,
        )
    }

    fn has(
        &self,
        core: &mut NativeCx<'_>,
        object: ObjectId,
        key: oxide_core::PropertyKey,
    ) -> Result<Option<bool>, Throw> {
        self.run(
            core,
            object,
            oxide_core::Value::object(object),
            |class, scope, data| {
                let key = scope.view().lift_key(key)?;
                class.has(scope, data, &key)
            },
            keep,
        )
    }

    fn get(
        &self,
        core: &mut NativeCx<'_>,
        object: ObjectId,
        key: oxide_core::PropertyKey,
        receiver: oxide_core::Value,
    ) -> Result<Option<oxide_core::Value>, Throw> {
        self.run(
            core,
            object,
            receiver,
            |class, scope, data| {
                let key = scope.view().lift_key(key)?;
                class.get(scope, data, &key)
            },
            |view, value| view.lower(&value),
        )
    }

    fn set(
        &self,
        core: &mut NativeCx<'_>,
        object: ObjectId,
        key: oxide_core::PropertyKey,
        value: oxide_core::Value,
        receiver: oxide_core::Value,
    ) -> Result<Option<bool>, Throw> {
        self.run(
            core,
            object,
            receiver,
            |class, scope, data| {
                let mut view = scope.view();
                let key = view.lift_key(key)?;
                let value = view.lift(value)?;
                drop(view);
                class.set(scope, data, &key, value)
            },
            keep,
        )
    }

    fn delete(
        &self,
        core: &mut NativeCx<'_>,
        object: ObjectId,
        key: oxide_core::PropertyKey,
    ) -> Result<Option<bool>, Throw> {
        self.run(
            core,
            object,
            oxide_core::Value::object(object),
            |class, scope, data| {
                let key = scope.view().lift_key(key)?;
                class.delete(scope, data, &key)
            },
            keep,
        )
    }

    fn own_keys(
        &self,
        core: &mut NativeCx<'_>,
        object: ObjectId,
    ) -> Result<Option<Vec<oxide_core::PropertyKey>>, Throw> {
        self.run(
            core,
            object,
            oxide_core::Value::object(object),
            |class, scope, data| class.own_keys(scope, data),
            |view, keys| keys.iter().map(|key| view.key(key)).collect(),
        )
    }
}

impl View<'_> {
    pub(crate) fn register_exotic_class<C: ExoticClass>(
        &mut self,
        class: C,
    ) -> JsResult<HostClass<C>> {
        let shared = Rc::downgrade(self.shared);
        let hooks = Rc::new(Hooks {
            shared: Weak::clone(&shared),
            class,
        });
        let id = self.core.register_exotic_class(hooks);
        Ok(HostClass {
            id,
            shared,
            class: PhantomData,
        })
    }

    pub(crate) fn new_host_exotic<C: ExoticClass>(
        &mut self,
        value: C::Data,
        class: &HostClass<C>,
        prototype: Option<&ObjectRef>,
    ) -> JsResult<ObjectRef> {
        if !std::ptr::eq(class.shared.as_ptr(), Rc::as_ptr(self.shared)) {
            return Err(RuntimeError::Invariant("a class of another runtime"));
        }
        let prototype = prototype.map(|p| self.object(p)).transpose()?;
        let object = self
            .core
            .new_host_exotic(class.id, prototype, Rc::new(value))?;
        Ok(self.lift_object(object))
    }

    /// A complete descriptor in the core's form, its values rooted until
    /// the view drops.
    fn lower_complete_descriptor(
        &mut self,
        descriptor: &CompleteOrdinaryPropertyDescriptor,
    ) -> JsResult<oxide_core::PropertyDescriptor> {
        let accessor = |function: &Option<CallableRef>| {
            DescriptorField::Present(match function {
                Some(function) => AccessorValue::Callable(function.clone()),
                None => AccessorValue::Undefined,
            })
        };
        let partial = match descriptor {
            CompleteOrdinaryPropertyDescriptor::Data {
                value,
                writable,
                enumerable,
                configurable,
            } => OrdinaryPropertyDescriptor::data(
                value.clone(),
                *writable,
                *enumerable,
                *configurable,
            ),
            CompleteOrdinaryPropertyDescriptor::Accessor {
                get,
                set,
                enumerable,
                configurable,
            } => OrdinaryPropertyDescriptor {
                get: accessor(get),
                set: accessor(set),
                enumerable: DescriptorField::Present(*enumerable),
                configurable: DescriptorField::Present(*configurable),
                ..OrdinaryPropertyDescriptor::default()
            },
        };
        self.lower_descriptor(&partial)
    }

    /// A possibly partial descriptor the core validated
    /// (`ToPropertyDescriptor`), as the API's.
    fn lift_partial_descriptor(
        &mut self,
        descriptor: &oxide_core::PropertyDescriptor,
    ) -> JsResult<OrdinaryPropertyDescriptor> {
        let accessor = |view: &mut Self, field: Option<oxide_core::Value>| {
            Ok::<_, RuntimeError>(match field {
                None => DescriptorField::Absent,
                Some(value) => DescriptorField::Present(match view.lift(value)? {
                    Value::Object(object) => AccessorValue::Callable(CallableRef(object)),
                    _ => AccessorValue::Undefined,
                }),
            })
        };
        Ok(OrdinaryPropertyDescriptor {
            value: match descriptor.value {
                Some(value) => DescriptorField::Present(self.lift(value)?),
                None => DescriptorField::Absent,
            },
            writable: descriptor.writable.into(),
            get: accessor(self, descriptor.get)?,
            set: accessor(self, descriptor.set)?,
            enumerable: descriptor.enumerable.into(),
            configurable: descriptor.configurable.into(),
        })
    }
}
