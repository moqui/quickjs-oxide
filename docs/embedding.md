# Embedding internals

This page records how JavaScript calls Rust in this engine: host functions and host objects, the operations a host function shares with the top-level `Context`, exceptions that carry their value, interrupts, promises settled from Rust, native modules, byte access to buffers and the conversion layer, with what each costs and the known issues. The API itself, as an embedder uses it, is in [`src/engine/api/README.md`](../src/engine/api/README.md); `examples/host.rs` runs every part of it. The native ABI, the rooting rule and the host boundary are in [`core/README.md`](../core/README.md), the loop in [Execution](performance/execution.md).

Every crate keeps `unsafe_code = "forbid"`, and a runtime has one realm.

## Where it lives

| Module | Crate | Content |
| --- | --- | --- |
| `builtins` (`natives!`) | `quickjs-oxide-core` | `Native::HostFunction`, `Native::HostConstructor` |
| `heap::object` | `quickjs-oxide-core` | `InternalData::HostFn`, `InternalData::HostData`, `InternalTag::HostData` |
| `vm::native` | `quickjs-oxide-core` | `HostFn`, `HostBody`, `NativeCall`, the trampoline `host_call` |
| `vm::exotic` | `quickjs-oxide-core` | `ExoticHooks`, `ExoticClass`, `HostExotic`, the hook dispatch `vm::object` calls for `ObjectKind::HostExotic` |
| `embed` | `quickjs-oxide-core` | `NativeCx`, which holds every embedder operation (`Runtime` delegates to it); host functions and objects, promise capabilities, the buffer operations, the interrupt flag |
| `vm::error`, `vm::interp`, `bytecode` | `quickjs-oxide-core` | `vm.interrupted`, `poll_interrupts`, `catchable`, the uncatchable unwind, `Opcode::JmpBack`, the poll counter |
| `host` | `quickjs-oxide-core` | `Host::interrupt` |
| `vm::module` | `quickjs-oxide-core` | `ModuleSource::Synthetic` |
| `api::core::ops` | root crate | `View`, the `js_ops!` macro, the `Js` trait |
| `api::core::function` | root crate | `Scope`, `FunctionOptions`, `HostFunction`, the body wrapper |
| `api::core::exotic` | root crate | `ExoticClass`, `Hook`, `HostClass`, `ExoticScope`, the hooks wrapper |
| `api::core::value`, `api::core::runtime` | root crate | `Weak` slots, `Poisoned`, `InterruptHandle` |
| `api::core::error` | root crate | `Exception(Value)`, `Interrupted`, `Poisoned` |
| `api::core::module` | root crate | `ModuleScope`, `ModuleSource::Synthetic`, `Context::define_module` |
| `api::core::promise`, `api::core::buffer`, `api::core::convert` | root crate | `PromiseCapability`; `TypedArrayView`, `TypedArrayKind`; `FromJs`, `IntoJs` and the typed `HostFunction` impls |
| `src/engine/compiler/register` | root crate | the lowering emits `JmpBack` for a backward `Jmp` |

The tests are `core/tests/host.rs` (the core's host functions and objects), `tests/embedding_api.rs` (the API), `tests/host_exotic.rs` (host exotic objects) and `tests/reentry.rs` (lending the engine).

## Host functions in the core

**Two natives.** `Native::HostFunction` and `Native::HostConstructor` sit at the end of `natives!` with the trampoline as their function. `HostConstructor` is declared a constructor, so `new` of a `HostFunction` throws `TypeError: not a constructor` through the existing check. Neither is frameless: a host function named `h` is listed in a backtrace as `at h (native)`, as QuickJS lists a C function.

**The body lives in the data object.** A host function is a native closure. Its `FunctionRecord::scope` is an `InternalTag::NativeData` record whose slots hold the JavaScript values the embedder captured (traced like every internal slot), and whose data is `InternalData::HostFn(HostBody)`, an `Rc<dyn HostFn>`. A host object's datum is `InternalData::HostData(Rc<dyn Any>)` on the same kind of record. `Internal::owned_bytes` counts both as 0. The sweep writes `Internal::vacant()` over a freed record, so a body or a datum drops when its object is collected, or at the runtime's drop; the function object's lifetime is the closure's lifetime, with no side table.

The body is an `Rc` because a host function is re-entrant: JavaScript calls it, it calls back into JavaScript, which calls it again. The trampoline clones the `Rc` out of the record, which ends the heap borrow, and calls through the clone. Mutable state goes in a `Cell` or `RefCell` inside the closure.

**The trampoline.** `host_call` reads the body from the callee's data record and calls it with a `NativeCx` and a `NativeCall` (the call site: receiver, arguments, `new.target`, callee, captured values). The arguments stay in the register file, rooted by `top`; `native::call` cuts the root stack back when the body returns, so a body roots freely. `host_call` enters through `enter_native`, so a host function that recurses into JavaScript past the native-stack budget throws `InternalError: stack overflow` as a builtin does.

**`NativeCx`.** A host function runs inside an entry that has a `Cx` and no `Runtime`, so the embedder's operations are on `NativeCx`: the heap, the realm, the services, and the code store, owned (`Runtime::native_cx`) or shared (inside a host function, while the loop holds it). `NativeCx::compiler(transient)` compiles into the code store unless `transient`, between entries and inside a host function alike: the store only grows and never moves a unit anything can borrow, which makes publishing safe while the running loop borrows the unit it runs. `CodeStore::publish` takes `&self` and puts the unit in the store's overflow, 256 chunks of 128 `OnceCell`s filled once; the loop reads the settled units, a plain vector, and `NativeCx::cx` and `Runtime::cx` move the overflow into it (`CodeStore::settle`, ids unchanged) whenever the store is not shared, between entries. A unit published inside a host function therefore runs through the overflow's lookup until the entry returns, and at the static code's speed from then on: on V8 v7 DeltaBlue, compiled by one host function and run by a later one it retires the instructions of a top-level compile (17.817 G against 17.816 G), and compiled and run inside the same call 6% more (transient code: 12% more). Past the overflow's 32,768 units a compilation inside a host function goes on transiently, its atoms held rather than pinned; between entries the store is the runtime's alone and a compilation appends straight to the settled units, with no bound. Code compiled inside a host function without `transient` is therefore permanent, as top-level code is: right for an embedder's cached script texts, and `CompileOptions::transient` (or `EvalOptions::transient`) keeps one-off text collectable. A unit published inside a host function and run in that same entry reads its unit through the overflow's out-of-line lookup on each call and return; there is no cheap point to settle it earlier, since the running loop borrows the store until the outermost entry returns. The call path takes the callee's unit from its lookup instead of looking it up again. `new_host_function` allocates the data record and the closure and defines `name` and `length`; a constructor gets no `prototype` property, which the embedder defines, as QuickJS's `JS_NewCFunction2` leaves it to `JS_SetConstructor`.

## Host objects

A host object is an `ObjectKind::Builtin` object with the embedder's prototype and an `InternalTag::HostData` record. It prints as `Object { … }`, is `[object Object]` to `Object.prototype.toString` unless its prototype defines `Symbol.toStringTag`, and is `typeof` `"object"`. `host_data` returns the datum as an `Rc`, so the API hands out a clone without borrowing the heap across the embedder's code. A `Drop` of a datum that releases an `ObjectRef` of the same runtime is deferred like any release made while the runtime is borrowed; a `Drop` that calls an API method gets `RuntimeError::Reentered`.

## Host exotic objects

A host exotic object's property operations are the embedder's Rust hooks, as QuickJS's `JSClassExoticMethods` are: `[[GetOwnProperty]]`, `[[DefineOwnProperty]]`, `[[HasProperty]]`, `[[Get]]` and `[[Set]]` with their receiver, `[[Delete]]` and `[[OwnPropertyKeys]]`. Its uses are a resolver of bare global names (the global object's prototype), records whose fields live in Rust, and maps converted on first read.

**The class.** The embedder implements `ExoticClass` (its `Data` type and the hooks it needs) and registers it once with `register_exotic_class`, which returns a `HostClass<C>`; `new_host_exotic(value, &class, prototype)` makes an object of it, and `host_data::<C::Data>` reads its datum back as for a host object. In the core a class is an index into `Services::exotic_classes` (an `Rc<dyn ExoticHooks>` each, living as long as the runtime), and an object is an `ObjectKind::HostExotic` with an `InternalTag::HostData` record whose datum is `InternalData::HostExotic { class, data }`: the class costs the record no space beyond the host object's.

**Declining.** Each hook returns `Hook::Handled(answer)` or `Hook::Ordinary`, which runs the ordinary internal method on the object, as QuickJS does for a method a class leaves `NULL`. `get`, `set` and `has` answer for the whole chain; when they decline, the ordinary method asks `get_own_property` (whose own decline reads the object's ordinary properties) and continues to the prototype. A `set` that declines is `OrdinarySet`, which ends in the receiver's `[[GetOwnProperty]]` and `[[DefineOwnProperty]]`; a class that answers only `get_own_property` and `define_own_property` is complete. A `define_own_property` or `delete` that declines is the ordinary method over the class's `[[GetOwnProperty]]` answer (`ValidateAndApplyPropertyDescriptor`, `OrdinaryDelete`), so a property it reports non-configurable stays; an accepted definition is stored ordinarily. `own_keys`' list and the ordinary own keys are merged without repeats in an ordinary object's order: integer indices ascending, then strings, then symbols. The engine checks none of a proxy's invariants against the answers.

```rust
struct Names;
impl ExoticClass for Names {
    type Data = RefCell<HashMap<JsString, Value>>;
    fn has(&self, _: &mut ExoticScope<'_>, map: &Self::Data, key: &PropertyKey) -> JsResult<Hook<bool>> {
        let found = key.to_js_string().is_some_and(|name| map.borrow().contains_key(&name));
        // Not ours: `Object.prototype` behind the object answers.
        Ok(if found { Hook::Handled(true) } else { Hook::Ordinary })
    }
    fn get(&self, _: &mut ExoticScope<'_>, map: &Self::Data, key: &PropertyKey) -> JsResult<Hook<Value>> {
        let found = key.to_js_string().and_then(|name| map.borrow().get(&name).cloned());
        Ok(found.map_or(Hook::Ordinary, Hook::Handled))
    }
}
let class = context.register_exotic_class(Names)?;
let names = context.new_host_exotic(RefCell::default(), &class, Some(&context.object_prototype()?))?;
context.set_prototype_of(&context.global_object()?, Some(&names))?;
```

`examples/exotic_globals.rs` runs this with a `set` and times it.

**`ExoticScope`.** A hook runs with an `ExoticScope`, a third handle beside `Context` and `Scope` that `js_ops!` gives every operation of `Js`, so a hook makes values, calls functions, evaluates and throws as a host function does. It reads the exotic object (`this`) and the `[[Get]]` or `[[Set]]` receiver (`receiver`, `receiver_is_this`) on demand, so a hook that does not ask makes no handle; the key arrives as a `PropertyKey`, copied once from the atom's flat text.

**In the engine.** `vm::object`'s internal methods take the hook path for the kind beside the `Proxy` arms. The kind is in `ObjectKind::is_exotic`, so `vm::property`'s heap-only walks, the named and global inline caches and the element fast paths leave for those methods when they meet it on an object or a prototype, and no cache entry is filled for it or through it: a hook's answer is read on every access. It is not in `has_elements`, so an index the ordinary fallback stores is a named property, which the element fast paths do not read. A bare name the global object does not hold goes, as through a `Proxy`, to `has` and then `get` (`typeof` of a name `has` denies is `"undefined"`); a strict assignment asks `has` before `set` and throws `ReferenceError` on `false`; a sloppy one calls `set` with the global object as receiver; `delete name` deletes from the global object and does not reach the prototype.

**The call.** `vm::exotic::invoke` clones the class's `Rc` out of the services (the heap is free while it runs), enters through `enter_native` (so a hook that recurses through itself throws `InternalError: stack overflow`), roots the object, the key and the values passed, and cuts the root stack back when the hook returns. The API's wrapper is the host function wrapper's: it holds the runtime weakly, its guard poisons the runtime when a hook panics, it translates an error into a throw by the same table, and it releases the handles the hook dropped.

## The API layer

**One set of operations, two handles.** Every `Context` method borrows the runtime's cell for one entry, so inside a host function a `Context` method returns `Reentered`; a host function works through its `Scope`. Both run their operations on an internal `View`: a `NativeCx`, the runtime's shared state, and a root mark at which the root stack is cut back when the view drops, so temporaries a `lower` creates (a string, a BigInt, an atom) live until the operation ends. The macro `js_ops!` lists each operation once and emits the inherent methods of `Context` (`&self`, one entry each), the inherent methods of `Scope` (`&mut self`, a reborrow of its `NativeCx`), and the sealed trait `Js` for code generic over where it runs. A macro cannot give one list's methods different receivers, which is why every `Context` method takes `&self`.

The job queue, the module loader, the collector, the reports and the options are not in `Js`: they belong to the runtime boundary. A host function that needs the job queue either returns to the embedder, which drains it, or lends the engine and drains it from there ([Lending the engine](#lending-the-engine)). Inside a host function, `eval`, `compile`, `load_image`, `compile_module` and `execute_module` work as at the top level: their units go in the code store unless the call asks for transient code.

**`Scope` and `FunctionOptions`.** A `Scope` reads its call on demand (`this`, `arg`, `argc`, `args`, `new_target`, `is_construct`, `callee`, `data`, `set_data`, `this_host_data`), so a function that takes three integers makes no handle and copies no string. `FunctionOptions` gives the `name`, `length`, whether `new` may call the function, and its captured values. A closure passed to `new_function` writes its parameter type (`|scope: &mut Scope<'_>|`): the bound is reached through the `HostFunction` impl, and the compiler does not infer the higher-ranked signature from it.

**The body wrapper.** The API registers, as the core's body, a wrapper holding a `Weak<Shared>` (the wrapper lives inside the runtime it names) and the embedder's `HostFunction`. It builds the `Scope`, calls the function, and translates the result:

| The function returns | The core receives |
| --- | --- |
| `Ok(value)` | `lower(value)`; a constructor's non-object result passes through, as in QuickJS |
| `Err(Exception(value))` | `Throw(lower(value))` |
| `Err(Interrupted)` | the interrupt's throw, with `vm.interrupted` still set |
| `Err(OutOfMemory)` | `Throw(%OutOfMemoryError%)` |
| `Err(Invariant(_))`, `Err(Internal(_))`, `Err(Unsupported(_))`, `Err(Compile(_))` | `Throw` of `InternalError: <the error's Display>` |

After each call it moves the handles released during the call into the core's handle table, so a script that calls a host function millions of times with object arguments keeps the handles of one call, not of all of them. A guard whose `Drop` sees `std::thread::panicking()` marks the runtime poisoned.

**Handles do not keep the runtime alive.** `Slot` holds a `Weak<Shared>`. A closure stored in the heap that captured an `ObjectRef` would otherwise make the cycle closure → handle → runtime → heap → closure, and the runtime would never drop. A value used after its runtime dropped gets `RuntimeError::Invariant("a value of a dropped runtime")`. A captured handle is a root while its function lives; a JavaScript value the collector should see goes in `FunctionOptions::data`.

## Lending the engine

A host function's `Scope` is a borrow of the running loop's engine, and safe Rust gives code deeper on the stack no way to reach a borrow that was not passed to it. An embedder whose host function calls its own service layer, which runs another script on the same runtime with only a `Context` in hand (script → host function → service → script), therefore gets `Reentered` from that `Context`. `Scope::lend(f)` passes the engine on through the runtime instead: while `f` runs, the runtime's `Context` and `Runtime` operations enter it.

```rust
context.define_function(&global, "callService", 1, move |scope: &mut Scope<'_>| {
    let name = scope.arg(0);
    let name = scope.to_js_string(&name)?.to_utf8_lossy();
    // `services` holds a Context; it runs scripts, calls functions and
    // drains jobs while lent.
    scope.lend(|| services.call(&name))
})?;
```

**The mechanism.** `NativeCx::lend` swaps the heap and the services out of the host function's view into a core `Lent`, leaving a `Spare`'s empty heap and default services behind, and clones the shared owners of the code store and the realm (`Runtime::code` and `Runtime::realm` are `Rc`s for this; `Cx` carries `code_owner` and `realm_owner` beside the plain borrows the loop reads). The API puts the `Lent` in `Shared::lent`; `Runtime::engine`, which every `Context` operation and the runtime's value, property and job operations use, starts a top-level entry when the runtime's cell is free and otherwise takes the `Lent` for the duration of the entry (`LentEntry`), building a `NativeCx` over it whose code view is shared, as a host function's is. When `f` returns or unwinds, a guard swaps the heap and the services back and keeps the spare for the next lend. Nothing is copied but the two structures' headers: the frames, the register file, the root stack and the handle table live in the heap, so a nested call runs above the host function's frames exactly as a call from its `Scope` would, against the same native-stack budget. A host function a nested script calls can lend again. A host exotic object's hook lends the same way, through `ExoticScope::lend`.

**What runs.** Inside a lend, as inside a host function: compiled and loaded code and modules go in the code store as at the top level, an exception returns as `RuntimeError::Exception` and an interrupt as `RuntimeError::Interrupted` with the flag still set (the host function returns it to keep unwinding; no `catch` runs), and `run_pending_job`, `run_jobs` and `is_job_pending` work on the shared queue (a nested job leaves `ClearKeptObjects` to the outermost script or job). Collection, the reports, the options, `define_module` and the Test262 host stay at the top level and return `Reentered`. `Runtime::is_lent` reports a lend in progress. A panic in `f` unwinds through the guard, which gives the engine back, and the host function's poison guard poisons the runtime.

**Cost.** In a release build a host function that lends and returns costs about 0.2 µs more than one that does not (30 ns): the two swaps of the heap's and the services' headers into a boxed spare. A nested `eval("1")` through the lend costs about 3.3 µs.

**`ModuleScope`.** The module loader's callback view is the compile-time view: it has no services and cannot build a `NativeCx`, so it does not implement `Js`. It makes objects, errors, functions and host objects, and compiles modules, which is what a loader needs to return a compiled or a synthetic module.

**Poisoning.** A panic in a host function unwinds through the loop, leaving its frames and `top` inconsistent. The wrapper's guard sets the runtime poisoned; every later entry returns `RuntimeError::Poisoned`. Nothing catches the panic: it reaches the embedder's call as a panic. The runtime drops normally.

## Code images

A `CodeImage` is a compiled script by value (`oxide_core::image`): each unit's instructions, handlers, captures, counts, flags, positions, source text and children, and its constant table, with every atom by text in one table, numbers by their bits, BigInts by sign and limbs, non-atom strings by their text and template site objects by their strings. A regular expression is two constants already (its source and flags atoms), so it needs nothing more. Nothing in an image names the runtime that made it, so it is `Send + Sync` and an `Arc` to clone: an embedder keeps one per script text for the process and every thread's runtime loads it.

`encode_image(&FunctionBytecodeRef)` walks the script's bytecode records from the root in the pre-order `instantiate` takes: for each its unit and its constant table. An instruction whose `imm` is a string key holds the atom's table index instead, and the unit lists those instructions (`key_sites`), so `load_image` copies the instructions and patches only the listed ones with the loading runtime's atoms, interned once per image atom; it decodes nothing. Only a script's root encodes: a closure (whose cells are its runtime's), any other function or object is `EncodeError::NotBytecode`, and a symbol key is refused. A load changes nothing until the whole image has passed two checks: `image::check` (the children form the image's pre-order, every atom, child and constant index is in range, the key sites are exactly the sorted instructions with string keys, the counts fit the code, the root is a script that captures no cell, and only a generator yields and only an async function awaits) and the publication validator over every unit, run on provisional units whose children are the image's own. Only then does it reserve the cache entries, intern atoms and publish the units from the last back (each child follows its parent in pre-order) without validating them again, and instantiate the tree with the rebuilt constant tables. `CodeImage::corrupted` (hidden) breaks one part of an image for the tests of each check. The units go in the code store, as a compiled script's do, at the top level and inside a host function alike. `compile_image(source, filename)` compiles transiently and encodes. `front_end_parses()` counts the roots this thread's front end has parsed, which is how the tests show that a load parses nothing.

**Shared bodies and instructions.** A `CodeUnit` keeps only what names its runtime or is read on every call: the counts and flags, the cache base, the children, the name atom, `length`, a dynamic unit's atoms, and its key table (`keys`). Everything else is a `UnitBody` behind an `Arc`, which the unit derefs to, so readers write `unit.code` or `unit.handlers` as before and builders fill it through `body_mut`: the instructions, handlers, captures, debug information (name, filename as an `Arc<str>`, positions, definition), source text and direct-eval sites. The instructions name nothing of the runtime: `CodeBuilder::finish` moves each key the instructions name into the unit's key table, once in order of first use, and leaves the operand its index, which `CodeUnit::key(insn)` reads (only the slow paths and the defining and deleting opcodes read a key; a cache hit reads none). Of the two ways #122 names, unit-local key tables cost one indexed load where a key is read; process-wide atom ids would make that read free but need one append-only atom table every runtime's heap honours, a far larger change to the heap, so this takes the key tables. An image holds each unit's body by its `Arc` and its key table by atom index; a load interns the image's atoms once, builds each unit's key table and clones one `Arc` per unit, copying no instruction. `CodeReport::static_bytes` counts what the code store holds for this runtime alone and `shared_body_bytes` the bodies held with an image or another runtime. On mr-mantle's `Forum.js` (201 KB, 249 functions), a compiled runtime holds 854 KB more; a runtime that loads its image holds 164 KB more: its code store 40 KB (units and key tables), the atoms and their table about 80 KB, the constant tables 32 KB; the 596 KB of bodies are the image's, once for the process. A unit's cache entries are reserved when it is first entered (`CodeUnit::ic_base` is a `Cell`, `IC_UNRESERVED` until `enter_frame` reserves it), so a function that never runs holds none: Forum.js's 5,315 entries (about 90 KB) are taken only by the functions a request runs. The load takes 0.40 ms against 11.3 ms to compile (release build, x86-64). On the V8 v7 programs the key tables and the deferred reservation cost DeltaBlue 1.1% more instructions, Richards 0.5% and RayTrace and EarleyBoyer about 0.3%. What remains per runtime is its atoms and constant tables; sharing those needs process-wide atoms.

Images are an in-memory form: there is no byte format yet ([planned](planned.md#4-code-images-and-heap-snapshots) section 4.2 describes one), and modules have no image. `tests/code_images.rs` covers every constant kind, closures, classes, generators and async functions, another runtime's atom numbering, error positions and threads; `examples/code_images.rs` compares compiling and loading a script.

## Exceptions carry their value

`RuntimeError::Exception(Value)` carries the thrown value; there is no pending-exception slot. A thrown value moves from a nested call to the script's `catch` through `?`, and `throw_error(kind, message)` builds an error object with a `stack` from the running frames and returns it as `Exception(value)`, so a host function throws with `return Err(scope.throw_error(NativeErrorKind::Type, "…"))`. The out-of-memory intrinsic still maps to `OutOfMemory`.

## Interrupts

**The poll.** The lowering emits `Opcode::JmpBack` for a `Jmp` whose target precedes it: every loop back-edge and nothing else. Its handler is `Jmp`'s plus a decrement of `vm.poll` and a branch to a cold function at 0; `enter_frame` decrements for every call, as QuickJS's `JS_CallInternal` does. The counter is in `VmStack`, not a local of `run_at`, where a new local spills `pc`. Forward jumps pay nothing.

The cold `poll_interrupts` resets the counter to `INTERRUPT_INTERVAL`, 1,000, then stops the script when the `InterruptHandle` flag is set or `Host::interrupt` returns `true`: it sets `vm.interrupted` and throws a fresh `InternalError: interrupted`. 1,000 costs the same instructions as QuickJS's 10,000 on the V8 v7 programs, and a shorter interval stops a script sooner.

**The hook and the handle.** `Host::interrupt` is polled at that interval; a deadline is `Instant::now() >= deadline` in it. `Runtime::interrupt_handle()` returns an `InterruptHandle` (an `Arc<AtomicBool>`, `Send + Sync`) whose `interrupt()` another thread calls. The flag stays set until `clear()`.

**Uncatchable.** While `vm.interrupted` is set, `run_at`'s throw path skips the handler search and pops every frame (`unwind_uncatchable`), so no `catch` and no `finally` runs, as QuickJS's `JS_IsUncatchableError` check does. The check sits before the throw loop and the unwind is a cold function: a filter on the handler search inside the loop cost 800 bytes of `run_at` and up to 3.5% of instructions. The places that turn a throw into a value test `error::catchable` and pass the throw on instead: the `Promise` constructor's executor and the promise resolution functions (`builtins::promise`, `vm::jobs`), a job's own throw, async function and async generator resumption (`vm::suspend`, `vm::async_generator`), generator resumption (`builtins::generator`), iterator close (`vm::iter`), and module evaluation (`vm::module`).

At the outermost entry the API clears `vm.interrupted` and returns `RuntimeError::Interrupted`, without the error object (reading it would allocate). An interrupted job stays off the queue and the jobs after it stay queued. A host function's nested call that is interrupted returns `Interrupted`, and the wrapper turns it back into the throw with the flag still set.

## Promises from Rust

`NativeCx::new_promise_capability` is `NewPromiseCapability(%Promise%)` over the existing `jobs::new_promise` and resolving functions. The API's `PromiseCapability` holds the promise and its `resolve` and `reject` functions; settling it calls them, so a thenable resolution queues its job as the specification says. `promise_then` attaches host functions as handlers. A host function returns `capability.promise` and keeps the capability in the embedder's state; when its event arrives, the embedder settles it from the top level and calls `run_jobs()`. Only the embedder drains the job queue, at the top level.

## Native modules

`ModuleSource::Synthetic { exports }` is a module whose exports are values: `vm::module::synthetic_module` (the generalization of the JSON module) makes one binding per export and sets each. An export named `default` is the default export. A `ModuleLoader` returns it for a specifier; `Context::define_module(name, exports)` stores it for the name without a loader, as `compile_module` stores a compiled one, and a second definition of a name replaces the first for later imports. Its namespace prints like a compiled module's.

## Buffers

`NativeCx` reads and writes an `ArrayBuffer`'s bytes over `heap::buffer::Buffer`: owned bytes, or atomic bytes in an `Arc` for a `SharedArrayBuffer`, read and written byte by byte as the builtins do. A detached buffer throws `TypeError: ArrayBuffer is detached`; an offset past the end throws `RangeError`. The API's `with_array_buffer_bytes` keeps the borrow inside one entry (the closure has no handle to call the API with) and copies a shared buffer first. `TypedArrayView` reports a typed array's kind, buffer, byte offset and current length, `None` when the view is out of bounds or the buffer is detached; `new_typed_array` constructs through the kind's intrinsic.

## Conversions

`FromJs` and `IntoJs` convert between JavaScript values and Rust types as the specification's abstract operations do: `bool` (`ToBoolean`), `i32` (`ToInt32`), `u32` (`ToUint32`), `f64` (`ToNumber`), `String` and `JsString` (`ToString`), `Option<T>` (`undefined` and `null` are `None`), `Vec<T>` (an array-like: `ToLength` of `length`, then each index), `ObjectRef`, `CallableRef`, and `Rc<T>` of a host datum. A conversion that runs JavaScript (a `valueOf`) runs it through the handle it is given. Where QuickJS has no message for a failed conversion, the message is the nearest builtin's: `not an object`, `not a function`, `<Type> object expected`.

`HostFunction` is implemented for closures over a `Scope` and up to eight converted arguments: a missing argument converts `undefined`, extra ones are ignored, and the result converts by `IntoJs`.

## Strings across the boundary

The core lends a string's flat representation (`NativeCx::string_repr`, which flattens a rope); the API copies that slice once into a `JsString`, and a `JsString` passed back is allocated from its flat storage. `JsString::to_utf8_lossy` takes ASCII and Latin-1 bytes and flat UTF-16 directly. Every string an embedder reads goes this way: host function arguments, `eval` results and property values.

## Measurements

On the V8 v7 programs at full fixed work, against the same tree without this layer (`instructions:u`): richards +0.71%, deltablue +0.93%, crypto +0.23%, raytrace +0.44%, earley-boyer +0.42%, regexp −0.03%, splay +0.21%, navier-stokes +0.72%. Host functions and objects alone move nothing (within 0.02%); the rest is the interrupt poll. `run_at` is 15,210 bytes (gate: 16,384).

A host function call costs about 560 instructions with no arguments. With a 1 KiB string argument, per call:

| Argument type | Latin-1 or ASCII text | UTF-16 text |
| --- | ---: | ---: |
| `JsString` | ~430 | ~630 |
| `String` | ~1,300 | ~56,000 |

A read of a bare name through an exotic global prototype is two hooks (`has`, `get`), an add (`counter += 1`) three. In `examples/exotic_globals.rs` (release build, x86-64 desktop), 1,000 adds of a name a `HashMap<JsString, Value>` holds take about 220 µs, 1,000 reads 145 µs and 1,000 writes 85 µs, against 12 µs for 1,000 adds of an ordinary global; most of the difference is the hooks' conversions and the embedder's hash lookups.

## Known issues

- **Recursion depth.** Rust → JavaScript → Rust recursion through a host function fits about 725 levels in the default 1 MiB native-stack budget in a release build and about 46 in a debug build; a builtin's re-entry reaches more (`Array.prototype.map`: 1,063 in release). The 1,000-level test runs on a thread with a 256 MiB stack.
- **UTF-16 text to `String`.** Converting wide text to a Rust `String` costs about 55 instructions per code unit, in std's `String::from_utf16_lossy`. A host function that only reads or forwards text takes `JsString` instead.
- **Interrupt latency.** The poll runs at back-edges and calls only. One long native operation (a regular expression over a large input, a large `join`) is not interrupted until it returns to the loop.
- **The interrupt flag stays set.** `InterruptHandle::interrupt` stops every later evaluation until `clear()`.
- **Host exotic objects print their ordinary properties.** The printer reads the heap and calls no hook; `console.log` of one lists only what the ordinary fallback stored.
- **An exotic prototype disables the hole fast path.** As for a `Proxy` or a prototype with index properties, a host exotic object used as a prototype clears `holes_writable`, so a write into an array hole walks the chain heap-wide from then on.
- **Host objects print as `Object`.** The printer does not use the prototype's `constructor`; `Symbol.toStringTag` changes only `Object.prototype.toString`.
- **`SharedArrayBuffer` bytes are copied.** `with_array_buffer_bytes` copies a shared buffer before calling the closure, since its bytes can change under the borrow.
- **A lend is explicit.** A `Context` call from Rust code a host function calls still returns `Reentered` unless the host function lends its engine around that code with `Scope::lend`; the scope is unusable while it is lent.
- **One thread per runtime.** Handles, host closures and host data are `Rc`; a thread pool creates a runtime on each worker.
- **Untested behavior.** No test pins a host function's backtrace line (`at h (native)`) or the handle table's size over many calls with object arguments; both behave as described.
- **`gc-stress`.** Two older tests in `tests/embedding_api.rs` are ignored under it: filling an 8 MiB heap cap takes minutes when every allocation collects, and a manual GC policy cannot hold off forced collections.
