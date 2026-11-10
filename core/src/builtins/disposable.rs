//! Explicit resource management, a fork addition (deviation ledger,
//! `FORK-EXPLICIT-RESOURCE-MANAGEMENT-001`): the dispose capability behind
//! `using` and `await using` declarations, `DisposableStack`,
//! `AsyncDisposableStack`, `SuppressedError`,
//! `%IteratorPrototype%[@@dispose]` and
//! `%AsyncIteratorPrototype%[@@asyncDispose]`.
//!
//! A dispose capability (`NewDisposeCapability`) is an internal record whose
//! slots are the completion so far (whether it is a throw, and its value),
//! `DisposeResources`' `needsAwait` and `hasAwaited` flags, then one
//! `(value, method, hint)` triple per resource. [`step`] runs
//! `DisposeResources` up to its next `Await`, so the same algorithm serves
//! the bytecode (a `using` scope's exit awaits each step itself) and
//! `AsyncDisposableStack.prototype.disposeAsync` (promise reactions resume
//! it).

use super::{
    Native, getter, install_constructor, link_constructor, methods, native_closure, native_data,
    native_pending, to_string_tag,
};
use crate::heap::HeapError;
use crate::heap::object::{InternalData, InternalTag, ObjectKind, flags};
use crate::runtime::{Intrinsic, Names, WellKnown};
use crate::shape::PropertyFlags;
use crate::value::{ObjectId, Value};
use crate::vm::error::{self as vm_error, ErrorKind};
use crate::vm::native::CallSite;
use crate::vm::object as internal;
use crate::vm::{Cx, Throw, jobs, ops};

/// Whether the completion so far is a throw completion.
const THROWING: usize = 0;
/// The thrown value when [`THROWING`].
const ERROR: usize = 1;
/// `DisposeResources`' `needsAwait`.
const NEEDS_AWAIT: usize = 2;
/// `DisposeResources`' `hasAwaited`.
const HAS_AWAITED: usize = 3;
/// The first resource triple: the value, the method, then the hint
/// (`true` for async-dispose).
const RESOURCES: usize = 4;

/// A stack object's slots: its state (`true` once disposed), then its
/// capability.
const STACK_DISPOSED: usize = 0;
const STACK_CAPABILITY: usize = 1;

fn root(cx: &mut Cx<'_>, value: Value) {
    cx.heap.roots().push(value);
}

pub(super) fn install(cx: &mut Cx<'_>) -> Result<(), HeapError> {
    install_stack(cx, false)?;
    install_stack(cx, true)?;
    let iterator_prototype = cx.realm.intrinsic(Intrinsic::IteratorPrototype);
    let dispose = cx.realm.symbols.dispose;
    super::symbol_method(
        cx,
        iterator_prototype,
        dispose,
        Native::IteratorPrototypeDispose,
        PropertyFlags::data(true, false, true),
    )?;
    let async_iterator_prototype = cx.realm.intrinsic(Intrinsic::AsyncIteratorPrototype);
    let async_dispose = cx.realm.symbols.async_dispose;
    super::symbol_method(
        cx,
        async_iterator_prototype,
        async_dispose,
        Native::AsyncIteratorPrototypeAsyncDispose,
        PropertyFlags::data(true, false, true),
    )
}

/// `DisposableStack` or `AsyncDisposableStack` with its prototype, in the
/// specification's property order.
fn install_stack(cx: &mut Cx<'_>, is_async: bool) -> Result<(), HeapError> {
    let object_prototype = cx.realm.intrinsic(Intrinsic::ObjectPrototype);
    let prototype = super::plain_object(cx, Some(object_prototype), 8)?;
    let (intrinsic, constructor, before, dispose, disposed, after, symbol, tag) = if is_async {
        (
            Intrinsic::AsyncDisposableStackPrototype,
            Native::AsyncDisposableStackConstructor,
            [
                Native::AsyncDisposableStackAdopt,
                Native::AsyncDisposableStackDefer,
            ],
            Native::AsyncDisposableStackDisposeAsync,
            Native::AsyncDisposableStackDisposed,
            [
                Native::AsyncDisposableStackMove,
                Native::AsyncDisposableStackUse,
            ],
            cx.realm.symbols.async_dispose,
            "AsyncDisposableStack",
        )
    } else {
        (
            Intrinsic::DisposableStackPrototype,
            Native::DisposableStackConstructor,
            [Native::DisposableStackAdopt, Native::DisposableStackDefer],
            Native::DisposableStackDispose,
            Native::DisposableStackDisposed,
            [Native::DisposableStackMove, Native::DisposableStackUse],
            cx.realm.symbols.dispose,
            "DisposableStack",
        )
    };
    cx.set_intrinsic(intrinsic, prototype);
    let function = install_constructor(cx, constructor)?;
    methods(cx, prototype, &before)?;
    // `@@dispose` (`@@asyncDispose`) is the same function object as
    // `dispose` (`disposeAsync`).
    let dispose_function = native_pending(cx, dispose, None)?;
    super::define(
        cx,
        prototype,
        dispose.js_name(),
        Value::object(dispose_function),
        super::hidden(),
    )?;
    getter(cx, prototype, "disposed", disposed)?;
    methods(cx, prototype, &after)?;
    super::define_key(
        cx,
        prototype,
        WellKnown::key(symbol),
        Value::object(dispose_function),
        super::hidden(),
    )?;
    to_string_tag(cx, prototype, tag)?;
    link_constructor(cx, function, prototype)
}

// ---- the dispose capability ----

/// `NewDisposeCapability()`.
pub(crate) fn new_capability(cx: &mut Cx<'_>) -> Result<ObjectId, Throw> {
    cx.heap
        .alloc_internal(
            ObjectKind::Builtin,
            InternalTag::DisposeCapability,
            None,
            0,
            &[Value::FALSE, Value::UNDEFINED, Value::FALSE, Value::FALSE],
            InternalData::None,
            0,
        )
        .map_err(|error| vm_error::heap_error(cx, error))
}

fn push_resource(
    cx: &mut Cx<'_>,
    capability: ObjectId,
    value: Value,
    method: Value,
    is_async: bool,
) -> Result<(), Throw> {
    let mark = cx.heap.roots().mark();
    root(cx, value);
    root(cx, method);
    let result = (|| {
        cx.heap.push_internal_slot(capability, value)?;
        cx.heap.push_internal_slot(capability, method)?;
        cx.heap
            .push_internal_slot(capability, Value::bool(is_async))
    })();
    cx.heap.roots().truncate(mark);
    result.map_err(|error| vm_error::heap_error(cx, error))
}

/// `AddDisposableResource(capability, value, hint)` for a declaration or a
/// stack's `use`: `CreateDisposableResource` reads the dispose method.
pub(crate) fn add_resource(
    cx: &mut Cx<'_>,
    capability: ObjectId,
    value: Value,
    is_async: bool,
) -> Result<(), Throw> {
    if value.is_undefined() || value.is_null() {
        // A null resource of `await using` still makes the scope's exit
        // await once.
        if is_async {
            push_resource(cx, capability, Value::UNDEFINED, Value::UNDEFINED, true)?;
        }
        return Ok(());
    }
    if value.as_object().is_none() {
        return Err(vm_error::type_error(cx, "not an object"));
    }
    let mark = cx.heap.roots().mark();
    root(cx, value);
    let result = (|| {
        let Some(method) = dispose_method(cx, value, is_async)? else {
            return Err(vm_error::type_error(
                cx,
                if is_async {
                    "no Symbol.asyncDispose or Symbol.dispose method"
                } else {
                    "no Symbol.dispose method"
                },
            ));
        };
        push_resource(cx, capability, value, method, is_async)
    })();
    cx.heap.roots().truncate(mark);
    result
}

/// `GetDisposeMethod(value, hint)`: an async hint falls back to `@@dispose`,
/// wrapped so that its result is not awaited and its throw rejects.
fn dispose_method(cx: &mut Cx<'_>, value: Value, is_async: bool) -> Result<Option<Value>, Throw> {
    let symbols = cx.realm.symbols;
    if is_async {
        let key = WellKnown::key(symbols.async_dispose);
        if let Some(method) = internal::get_method(cx, value, key)? {
            return Ok(Some(method));
        }
    }
    let key = WellKnown::key(symbols.dispose);
    let Some(method) = internal::get_method(cx, value, key)? else {
        return Ok(None);
    };
    if !is_async {
        return Ok(Some(method));
    }
    root(cx, method);
    let data = native_data(cx, &[method])?;
    let closure = native_closure(cx, Native::DisposeAsyncFromSync, data)?;
    Ok(Some(Value::object(closure)))
}

/// The closure `GetDisposeMethod` makes of a sync `@@dispose` for an async
/// hint: the method's throw rejects, and its result is not awaited.
pub(super) fn dispose_async_from_sync(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    let method = site.data(cx, 0);
    let this = site.this(cx);
    let [promise, resolve, reject] = intrinsic_capability(cx)?;
    match crate::vm::call(cx, method, this, &[]) {
        Ok(_) => {
            crate::vm::call(cx, resolve, Value::UNDEFINED, &[Value::UNDEFINED])?;
        }
        Err(Throw(reason)) if vm_error::catchable(cx) => {
            crate::vm::call(cx, reject, Value::UNDEFINED, &[reason])?;
        }
        Err(throw) => return Err(throw),
    }
    Ok(promise)
}

/// `AddDisposableResource(capability, undefined, hint, method)`.
fn add_method(
    cx: &mut Cx<'_>,
    capability: ObjectId,
    method: Value,
    is_async: bool,
) -> Result<(), Throw> {
    push_resource(cx, capability, Value::UNDEFINED, method, is_async)
}

/// Records a throw completion of a scope or of a disposal: the first one is
/// kept as is, a later one becomes a `SuppressedError` whose `error` is the
/// new value and whose `suppressed` is the completion so far.
pub(crate) fn record_error(
    cx: &mut Cx<'_>,
    capability: ObjectId,
    error: Value,
) -> Result<(), Throw> {
    if cx.heap.internal_slot(capability, THROWING) != Value::TRUE {
        cx.heap.set_internal_slot(capability, THROWING, Value::TRUE);
        cx.heap.set_internal_slot(capability, ERROR, error);
        return Ok(());
    }
    let suppressed = cx.heap.internal_slot(capability, ERROR);
    let combined = new_suppressed_error(cx, error, suppressed)?;
    cx.heap.set_internal_slot(capability, ERROR, combined);
    Ok(())
}

/// A newly created SuppressedError object with `error` and `suppressed`.
fn new_suppressed_error(cx: &mut Cx<'_>, error: Value, suppressed: Value) -> Result<Value, Throw> {
    let mark = cx.heap.roots().mark();
    root(cx, error);
    root(cx, suppressed);
    let result = (|| {
        let prototype = cx.realm.intrinsic(Intrinsic::SuppressedErrorPrototype);
        let object = cx
            .heap
            .alloc_object(ObjectKind::Ordinary, Some(prototype), 2)?;
        cx.heap.set_object_flag(object, flags::ERROR);
        root(cx, Value::object(object));
        define_error_fields(cx, object, error, suppressed)?;
        vm_error::add_stack(cx, object, false)?;
        Ok(Value::object(object))
    })();
    cx.heap.roots().truncate(mark);
    result.map_err(|error| vm_error::heap_error(cx, error))
}

fn define_error_fields(
    cx: &mut Cx<'_>,
    object: ObjectId,
    error: Value,
    suppressed: Value,
) -> Result<(), HeapError> {
    let names = cx.realm.names;
    let hidden = PropertyFlags::data(true, false, true);
    cx.heap
        .add_property(object, Names::key(names.error), hidden, error)?;
    cx.heap
        .add_property(object, Names::key(names.suppressed), hidden, suppressed)
}

/// Where [`step`] stopped.
pub(crate) enum Step {
    /// `Await(value)`, then step again (after [`record_error`] when it
    /// throws).
    Await(Value),
    /// Every resource is disposed; [`finish`] gives the completion.
    Done,
}

fn flag(cx: &Cx<'_>, capability: ObjectId, slot: usize) -> bool {
    cx.heap.internal_slot(capability, slot) == Value::TRUE
}

/// `DisposeResources` up to its next `Await`. A throw of a dispose method
/// is recorded; only an uncatchable one propagates.
pub(crate) fn step(cx: &mut Cx<'_>, capability: ObjectId) -> Result<Step, Throw> {
    loop {
        let length = cx.heap.internal(capability).slots.len();
        let pending_await = flag(cx, capability, NEEDS_AWAIT) && !flag(cx, capability, HAS_AWAITED);
        if length <= RESOURCES {
            if pending_await {
                cx.heap
                    .set_internal_slot(capability, NEEDS_AWAIT, Value::FALSE);
                return Ok(Step::Await(Value::UNDEFINED));
            }
            return Ok(Step::Done);
        }
        let is_async = cx.heap.internal_slot(capability, length - 1) == Value::TRUE;
        if !is_async && pending_await {
            // The resource is disposed by the next step.
            cx.heap
                .set_internal_slot(capability, NEEDS_AWAIT, Value::FALSE);
            return Ok(Step::Await(Value::UNDEFINED));
        }
        let value = cx.heap.internal_slot(capability, length - 3);
        let method = cx.heap.internal_slot(capability, length - 2);
        cx.heap.internal_mut(capability).slots.truncate(length - 3);
        if method.is_undefined() {
            cx.heap
                .set_internal_slot(capability, NEEDS_AWAIT, Value::TRUE);
            continue;
        }
        let mark = cx.heap.roots().mark();
        root(cx, value);
        root(cx, method);
        let result = crate::vm::call(cx, method, value, &[]);
        cx.heap.roots().truncate(mark);
        match result {
            Ok(result) if is_async => {
                cx.heap
                    .set_internal_slot(capability, HAS_AWAITED, Value::TRUE);
                return Ok(Step::Await(result));
            }
            Ok(_) => {}
            Err(Throw(error)) if vm_error::catchable(cx) => record_error(cx, capability, error)?,
            Err(throw) => return Err(throw),
        }
    }
}

/// The completion `DisposeResources` returns: the recorded throw, if any.
pub(crate) fn finish(cx: &mut Cx<'_>, capability: ObjectId) -> Result<(), Throw> {
    if !flag(cx, capability, THROWING) {
        return Ok(());
    }
    let error = cx.heap.internal_slot(capability, ERROR);
    cx.heap
        .set_internal_slot(capability, THROWING, Value::FALSE);
    cx.heap
        .set_internal_slot(capability, ERROR, Value::UNDEFINED);
    Err(Throw(error))
}

/// `DisposeResources` for a capability holding only sync-dispose resources
/// (which never await).
pub(crate) fn dispose_sync(cx: &mut Cx<'_>, capability: ObjectId) -> Result<(), Throw> {
    while let Step::Await(_) = step(cx, capability)? {}
    finish(cx, capability)
}

// ---- SuppressedError ----

/// `SuppressedError(error, suppressed, message)`.
pub(super) fn suppressed_error(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    let (error, suppressed, message) = (site.arg(cx, 0), site.arg(cx, 1), site.arg(cx, 2));
    let new_target = if site.construct {
        site.new_target
    } else {
        site.function
    };
    let prototype =
        internal::prototype_from_constructor(cx, new_target, Intrinsic::SuppressedErrorPrototype)?;
    let object = cx
        .heap
        .alloc_object(ObjectKind::Ordinary, Some(prototype), 3)
        .map_err(|error| vm_error::heap_error(cx, error))?;
    cx.heap.set_object_flag(object, flags::ERROR);
    root(cx, Value::object(object));
    if !message.is_undefined() {
        let message = ops::to_string(cx, message)?;
        root(cx, message);
        let key = Names::key(cx.realm.names.message);
        cx.heap
            .add_property(object, key, PropertyFlags::data(true, false, true), message)
            .map_err(|error| vm_error::heap_error(cx, error))?;
    }
    define_error_fields(cx, object, error, suppressed)
        .map_err(|error| vm_error::heap_error(cx, error))?;
    vm_error::add_stack(cx, object, true).map_err(|error| vm_error::heap_error(cx, error))?;
    Ok(Value::object(object))
}

// ---- DisposableStack and AsyncDisposableStack ----

fn construct_stack(cx: &mut Cx<'_>, site: &CallSite, is_async: bool) -> Result<Value, Throw> {
    if !site.construct {
        return Err(vm_error::type_error(cx, "constructor requires 'new'"));
    }
    let default = if is_async {
        Intrinsic::AsyncDisposableStackPrototype
    } else {
        Intrinsic::DisposableStackPrototype
    };
    let prototype = internal::prototype_from_constructor(cx, site.new_target, default)?;
    new_stack(cx, prototype, is_async)
}

fn new_stack(cx: &mut Cx<'_>, prototype: ObjectId, is_async: bool) -> Result<Value, Throw> {
    root(cx, Value::object(prototype));
    let capability = new_capability(cx)?;
    root(cx, Value::object(capability));
    stack_with(cx, prototype, is_async, capability)
}

fn stack_with(
    cx: &mut Cx<'_>,
    prototype: ObjectId,
    is_async: bool,
    capability: ObjectId,
) -> Result<Value, Throw> {
    let tag = if is_async {
        InternalTag::AsyncDisposableStack
    } else {
        InternalTag::DisposableStack
    };
    cx.heap
        .alloc_internal(
            ObjectKind::Builtin,
            tag,
            Some(prototype),
            0,
            &[Value::FALSE, Value::object(capability)],
            InternalData::None,
            0,
        )
        .map(Value::object)
        .map_err(|error| vm_error::heap_error(cx, error))
}

/// `RequireInternalSlot(this, [[DisposableState]])`.
fn this_stack(cx: &mut Cx<'_>, site: &CallSite, is_async: bool) -> Result<ObjectId, Throw> {
    let (tag, name) = if is_async {
        (InternalTag::AsyncDisposableStack, "AsyncDisposableStack")
    } else {
        (InternalTag::DisposableStack, "DisposableStack")
    };
    match site.this(cx).as_object() {
        Some(object) if cx.heap.internal_tag(object) == tag => Ok(object),
        _ => Err(vm_error::object_expected(cx, name)),
    }
}

fn is_disposed(cx: &Cx<'_>, stack: ObjectId) -> bool {
    cx.heap.internal_slot(stack, STACK_DISPOSED) == Value::TRUE
}

fn capability_of(cx: &Cx<'_>, stack: ObjectId) -> ObjectId {
    cx.heap
        .internal_slot(stack, STACK_CAPABILITY)
        .as_object()
        .expect("a dispose capability")
}

/// The receiver as a pending stack: a `ReferenceError` once disposed.
fn pending_stack(cx: &mut Cx<'_>, site: &CallSite, is_async: bool) -> Result<ObjectId, Throw> {
    let stack = this_stack(cx, site, is_async)?;
    if is_disposed(cx, stack) {
        return Err(vm_error::throw(
            cx,
            ErrorKind::Reference,
            "stack is already disposed",
        ));
    }
    Ok(stack)
}

fn adopt(cx: &mut Cx<'_>, site: CallSite, is_async: bool) -> Result<Value, Throw> {
    let stack = pending_stack(cx, &site, is_async)?;
    let (value, on_dispose) = (site.arg(cx, 0), site.arg(cx, 1));
    if !ops::is_callable(cx.heap, on_dispose) {
        return Err(vm_error::type_error(cx, "not a function"));
    }
    let data = native_data(cx, &[value, on_dispose])?;
    root(cx, Value::object(data));
    let closure = native_closure(cx, Native::DisposableStackAdoptCallback, data)?;
    let capability = capability_of(cx, stack);
    add_method(cx, capability, Value::object(closure), is_async)?;
    Ok(value)
}

/// The closure `adopt` adds: `onDispose(value)`.
pub(super) fn adopt_callback(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    let (value, on_dispose) = (site.data(cx, 0), site.data(cx, 1));
    crate::vm::call(cx, on_dispose, Value::UNDEFINED, &[value])
}

fn defer(cx: &mut Cx<'_>, site: CallSite, is_async: bool) -> Result<Value, Throw> {
    let stack = pending_stack(cx, &site, is_async)?;
    let on_dispose = site.arg(cx, 0);
    if !ops::is_callable(cx.heap, on_dispose) {
        return Err(vm_error::type_error(cx, "not a function"));
    }
    let capability = capability_of(cx, stack);
    add_method(cx, capability, on_dispose, is_async)?;
    Ok(Value::UNDEFINED)
}

fn disposed(cx: &mut Cx<'_>, site: CallSite, is_async: bool) -> Result<Value, Throw> {
    let stack = this_stack(cx, &site, is_async)?;
    Ok(Value::bool(is_disposed(cx, stack)))
}

fn move_(cx: &mut Cx<'_>, site: CallSite, is_async: bool) -> Result<Value, Throw> {
    let stack = pending_stack(cx, &site, is_async)?;
    let prototype = cx.realm.intrinsic(if is_async {
        Intrinsic::AsyncDisposableStackPrototype
    } else {
        Intrinsic::DisposableStackPrototype
    });
    let capability = capability_of(cx, stack);
    let moved = stack_with(cx, prototype, is_async, capability)?;
    root(cx, moved);
    let fresh = new_capability(cx)?;
    cx.heap
        .set_internal_slot(stack, STACK_CAPABILITY, Value::object(fresh));
    cx.heap
        .set_internal_slot(stack, STACK_DISPOSED, Value::TRUE);
    Ok(moved)
}

fn use_(cx: &mut Cx<'_>, site: CallSite, is_async: bool) -> Result<Value, Throw> {
    let stack = pending_stack(cx, &site, is_async)?;
    let value = site.arg(cx, 0);
    let capability = capability_of(cx, stack);
    add_resource(cx, capability, value, is_async)?;
    Ok(value)
}

pub(super) fn stack_constructor(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    construct_stack(cx, &site, false)
}

pub(super) fn stack_adopt(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    adopt(cx, site, false)
}

pub(super) fn stack_defer(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    defer(cx, site, false)
}

/// `DisposableStack.prototype.dispose()`.
pub(super) fn stack_dispose(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    let stack = this_stack(cx, &site, false)?;
    if is_disposed(cx, stack) {
        return Ok(Value::UNDEFINED);
    }
    cx.heap
        .set_internal_slot(stack, STACK_DISPOSED, Value::TRUE);
    let capability = capability_of(cx, stack);
    root(cx, Value::object(capability));
    dispose_sync(cx, capability)?;
    Ok(Value::UNDEFINED)
}

pub(super) fn stack_disposed(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    disposed(cx, site, false)
}

pub(super) fn stack_move(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    move_(cx, site, false)
}

pub(super) fn stack_use(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    use_(cx, site, false)
}

pub(super) fn async_stack_constructor(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    construct_stack(cx, &site, true)
}

pub(super) fn async_stack_adopt(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    adopt(cx, site, true)
}

pub(super) fn async_stack_defer(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    defer(cx, site, true)
}

pub(super) fn async_stack_disposed(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    disposed(cx, site, true)
}

pub(super) fn async_stack_move(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    move_(cx, site, true)
}

pub(super) fn async_stack_use(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    use_(cx, site, true)
}

/// `NewPromiseCapability(%Promise%)`, rooted.
fn intrinsic_capability(cx: &mut Cx<'_>) -> Result<[Value; 3], Throw> {
    let constructor = Value::object(cx.realm.intrinsic(Intrinsic::Promise));
    jobs::new_capability(cx, constructor)
}

/// `AsyncDisposableStack.prototype.disposeAsync()`.
pub(super) fn async_stack_dispose_async(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    let capability = intrinsic_capability(cx)?;
    let [promise, resolve, reject] = capability;
    let stack = match site.this(cx).as_object() {
        Some(object) if cx.heap.internal_tag(object) == InternalTag::AsyncDisposableStack => object,
        _ => {
            let Throw(error) = vm_error::object_expected(cx, "AsyncDisposableStack");
            crate::vm::call(cx, reject, Value::UNDEFINED, &[error])?;
            return Ok(promise);
        }
    };
    if is_disposed(cx, stack) {
        crate::vm::call(cx, resolve, Value::UNDEFINED, &[Value::UNDEFINED])?;
        return Ok(promise);
    }
    cx.heap
        .set_internal_slot(stack, STACK_DISPOSED, Value::TRUE);
    let resources = capability_of(cx, stack);
    let data = native_data(cx, &[Value::object(resources), resolve, reject])?;
    root(cx, Value::object(data));
    drive(cx, data)?;
    Ok(promise)
}

/// Runs `disposeAsync`'s `DisposeResources` to its next `Await`, whose
/// reactions resume it, or to its end, which settles the promise. The data
/// object holds the capability and the promise's resolving functions.
fn drive(cx: &mut Cx<'_>, data: ObjectId) -> Result<(), Throw> {
    let capability = cx
        .heap
        .internal_slot(data, 0)
        .as_object()
        .expect("a dispose capability");
    loop {
        let Step::Await(value) = step(cx, capability)? else {
            let (resolve, reject) = (
                cx.heap.internal_slot(data, 1),
                cx.heap.internal_slot(data, 2),
            );
            match finish(cx, capability) {
                Ok(()) => crate::vm::call(cx, resolve, Value::UNDEFINED, &[Value::UNDEFINED])?,
                Err(Throw(error)) => crate::vm::call(cx, reject, Value::UNDEFINED, &[error])?,
            };
            return Ok(());
        };
        // `Await(value)`: PromiseResolve, then the two reactions.
        root(cx, value);
        let constructor = Value::object(cx.realm.intrinsic(Intrinsic::Promise));
        let promise = match jobs::promise_resolve(cx, constructor, value) {
            Ok(promise) => promise,
            Err(Throw(error)) if vm_error::catchable(cx) => {
                record_error(cx, capability, error)?;
                continue;
            }
            Err(throw) => return Err(throw),
        };
        root(cx, promise);
        let on_fulfilled = native_closure(cx, Native::DisposeAsyncFulfilled, data)?;
        root(cx, Value::object(on_fulfilled));
        let on_rejected = native_closure(cx, Native::DisposeAsyncRejected, data)?;
        root(cx, Value::object(on_rejected));
        return jobs::perform_then(
            cx,
            promise.as_object().expect("a promise"),
            Value::object(on_fulfilled),
            Value::object(on_rejected),
            None,
            Value::UNDEFINED,
        );
    }
}

/// `disposeAsync`'s await resumed normally.
pub(super) fn dispose_async_fulfilled(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    let data = data_object(cx, &site);
    drive(cx, data)?;
    Ok(Value::UNDEFINED)
}

/// `disposeAsync`'s await threw: the throw joins the completion.
pub(super) fn dispose_async_rejected(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    let data = data_object(cx, &site);
    let capability = cx
        .heap
        .internal_slot(data, 0)
        .as_object()
        .expect("a dispose capability");
    let reason = site.arg(cx, 0);
    record_error(cx, capability, reason)?;
    drive(cx, data)?;
    Ok(Value::UNDEFINED)
}

fn data_object(cx: &Cx<'_>, site: &CallSite) -> ObjectId {
    let function = site.function.as_object().expect("a native closure");
    cx.heap
        .function_scope(function)
        .expect("a native closure's data")
        .object()
}

// ---- iterators ----

/// `%IteratorPrototype%[@@dispose]()`.
pub(super) fn iterator_dispose(cx: &mut Cx<'_>, site: CallSite) -> Result<Value, Throw> {
    let this = site.this(cx);
    let key = Names::key(cx.realm.names.return_);
    if let Some(method) = internal::get_method(cx, this, key)? {
        crate::vm::call(cx, method, this, &[])?;
    }
    Ok(Value::UNDEFINED)
}

/// `%AsyncIteratorPrototype%[@@asyncDispose]()`.
pub(super) fn async_iterator_async_dispose(
    cx: &mut Cx<'_>,
    site: CallSite,
) -> Result<Value, Throw> {
    let this = site.this(cx);
    let capability = intrinsic_capability(cx)?;
    let [promise, resolve, reject] = capability;
    let key = Names::key(cx.realm.names.return_);
    let result = (|| {
        let Some(method) = internal::get_method(cx, this, key)? else {
            return Ok(None);
        };
        let result = crate::vm::call(cx, method, this, &[Value::UNDEFINED])?;
        root(cx, result);
        let constructor = Value::object(cx.realm.intrinsic(Intrinsic::Promise));
        jobs::promise_resolve(cx, constructor, result).map(Some)
    })();
    match result {
        Ok(None) => {
            crate::vm::call(cx, resolve, Value::UNDEFINED, &[Value::UNDEFINED])?;
        }
        Ok(Some(wrapper)) => {
            root(cx, wrapper);
            let data = native_data(cx, &[])?;
            root(cx, Value::object(data));
            let unwrap = native_closure(cx, Native::AsyncIteratorDisposeUnwrap, data)?;
            root(cx, Value::object(unwrap));
            jobs::perform_then(
                cx,
                wrapper.as_object().expect("a promise"),
                Value::object(unwrap),
                Value::UNDEFINED,
                Some(capability),
                Value::UNDEFINED,
            )?;
        }
        Err(Throw(reason)) if vm_error::catchable(cx) => {
            crate::vm::call(cx, reject, Value::UNDEFINED, &[reason])?;
        }
        Err(throw) => return Err(throw),
    }
    Ok(promise)
}

/// The fulfillment handler of `[@@asyncDispose]`: `undefined`.
pub(super) fn async_iterator_dispose_unwrap(_: &mut Cx<'_>, _: CallSite) -> Result<Value, Throw> {
    Ok(Value::UNDEFINED)
}

// ---- the loop's `Dispose` instruction ----

/// [`Opcode::Dispose`](crate::bytecode::Opcode::Dispose) in the frame at
/// `base`.
#[inline(never)]
pub(crate) fn dispose_op(cx: &mut Cx<'_>, base: usize, insn: crate::Insn) -> Result<(), Throw> {
    use crate::bytecode::opcode::dispose;
    let at = |register: u8| base + register as usize;
    let capability = |cx: &Cx<'_>| {
        cx.heap.vm.registers[at(insn.b)]
            .as_object()
            .expect("a dispose capability")
    };
    let mark = cx.heap.roots().mark();
    let result = (|| match insn.imm {
        dispose::NEW => {
            let object = new_capability(cx)?;
            cx.heap.vm.registers[at(insn.a)] = Value::object(object);
            Ok(())
        }
        dispose::ADD_SYNC | dispose::ADD_ASYNC => {
            let value = cx.heap.vm.registers[at(insn.a)];
            let capability = capability(cx);
            add_resource(cx, capability, value, insn.imm == dispose::ADD_ASYNC)
        }
        dispose::RECORD_ERROR => {
            let error = cx.heap.vm.registers[at(insn.a)];
            let capability = capability(cx);
            record_error(cx, capability, error)
        }
        dispose::DISPOSE_SYNC => {
            let capability = capability(cx);
            dispose_sync(cx, capability)
        }
        dispose::STEP => {
            let capability = capability(cx);
            let (value, more) = match step(cx, capability)? {
                Step::Await(value) => (value, true),
                Step::Done => (Value::UNDEFINED, false),
            };
            cx.heap.vm.registers[at(insn.a)] = value;
            cx.heap.vm.registers[at(insn.c)] = Value::bool(more);
            Ok(())
        }
        _ => {
            let capability = capability(cx);
            finish(cx, capability)
        }
    })();
    cx.heap.roots().truncate(mark);
    result
}
