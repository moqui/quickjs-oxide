//! The embedding API: every method through the public surface.

use std::cell::RefCell;
use std::rc::Rc;

use quickjs_oxide::engine::api::core::{
    AccessorValue, CompleteOrdinaryPropertyDescriptor, Context, CoreValue, DescriptorField,
    GcPolicy, HeapLimits, Host, JsBigInt, JsString, ModuleImportAttributes, ModuleLoader,
    ModuleScope, ModuleSource, NativeErrorKind, OrdinaryPropertyDescriptor, PendingJobOutcome,
    PromiseState, QjsHelpers, RejectionEvent, Runtime, RuntimeError, RuntimeOptions, Stream, Value,
    WellKnownSymbol,
};

fn runtime() -> (Runtime, Context) {
    let runtime = Runtime::new();
    let context = runtime.context();
    (runtime, context)
}

/// The printed value `result` threw.
fn thrown<T: std::fmt::Debug>(context: &Context, result: Result<T, RuntimeError>) -> String {
    match result {
        Err(RuntimeError::Exception(exception)) => {
            context.runtime().print_value(&exception).unwrap()
        }
        other => panic!("not an exception: {other:?}"),
    }
}

#[test]
fn eval_returns_completion_values_of_every_kind() {
    let (_, context) = runtime();
    assert_eq!(context.eval("1 + 2").unwrap(), Value::Int(3));
    assert_eq!(context.eval("0.5").unwrap(), Value::Float(0.5));
    assert_eq!(
        context.eval("'a' + 'b'").unwrap(),
        Value::String(JsString::from_static("ab"))
    );
    assert_eq!(context.eval("true").unwrap(), Value::Bool(true));
    assert_eq!(context.eval("null").unwrap(), Value::Null);
    assert_eq!(context.eval("void 0").unwrap(), Value::Undefined);
    assert_eq!(
        context.eval("2n ** 70n").unwrap(),
        Value::BigInt(JsBigInt::parse_js_string("1180591620717411303424").unwrap())
    );
    assert!(matches!(
        context.eval("Symbol('s')").unwrap(),
        Value::Symbol(_)
    ));
    assert!(matches!(context.eval("({})").unwrap(), Value::Object(_)));
    // Declarations persist between scripts of the one realm.
    context.eval("var x = 41; let y = 1;").unwrap();
    assert_eq!(context.eval("x + y").unwrap(), Value::Int(42));
}

#[test]
fn errors_carry_the_thrown_value() {
    let (_, context) = runtime();
    let result = context.eval("null.x");
    assert!(
        thrown(&context, result)
            .starts_with("TypeError: cannot read property 'x' of null\n    at <eval> (<input>")
    );
    // The value is the thrown one, whatever its type.
    assert_eq!(
        context.eval("throw 7"),
        Err(RuntimeError::Exception(Value::Int(7)))
    );
    // A syntax error is thrown with its location, as `JS_Eval` throws it.
    let result = context.eval_with_filename("var x = ;", "f.js");
    assert_eq!(
        thrown(&context, result),
        "SyntaxError: unexpected token in expression: ';'\n    at f.js:1:9"
    );
    // A dropped feature (`with`) is an early SyntaxError.
    let result = context.eval("with ({}) {}");
    assert!(thrown(&context, result).starts_with("SyntaxError: with statements are not supported"));
}

#[test]
fn one_realm_per_runtime() {
    let runtime = Runtime::new();
    let first = runtime.new_context().unwrap();
    let second = Context::new(&runtime);
    first.eval("var shared = 7").unwrap();
    assert_eq!(second.eval("shared").unwrap(), Value::Int(7));
    assert_eq!(
        first.global_object().unwrap(),
        second.global_object().unwrap()
    );
    // Separate runtimes are isolated.
    let other = Runtime::new().context();
    assert_eq!(
        other.eval("typeof shared").unwrap(),
        Value::String(JsString::from_static("undefined"))
    );
}

#[test]
fn calls_construct_and_properties() {
    let (runtime, context) = runtime();
    let Value::Object(add) = context
        .eval("(function (a, b) { return this.base + a + b; })")
        .unwrap()
    else {
        panic!("a function");
    };
    let add = runtime.as_callable(&add).unwrap().expect("callable");
    let receiver = context.new_object().unwrap();
    let base = runtime.intern_property_key("base").unwrap();
    assert!(
        context
            .set_property(&receiver, &base, Value::Int(10))
            .unwrap()
    );
    assert_eq!(
        context
            .call(
                &add,
                Value::Object(receiver.clone()),
                &[Value::Int(1), Value::Int(2)]
            )
            .unwrap(),
        Value::Int(13)
    );
    let Value::Object(point) = context
        .eval("(class Point { constructor(x) { this.x = x; } })")
        .unwrap()
    else {
        panic!("a class");
    };
    let point = runtime.as_callable(&point).unwrap().unwrap();
    assert!(runtime.is_constructor(&point).unwrap());
    let Value::Object(instance) = context.construct(&point, &[Value::Int(5)]).unwrap() else {
        panic!("an instance");
    };
    let x = runtime.intern_property_key("x").unwrap();
    assert_eq!(context.get_property(&instance, &x).unwrap(), Value::Int(5));
    assert_eq!(
        runtime.get_prototype_of(&instance).unwrap(),
        Some(
            match context
                .get_property(&point, &runtime.intern_property_key("prototype").unwrap())
                .unwrap()
            {
                Value::Object(prototype) => prototype,
                _ => panic!("a prototype"),
            }
        )
    );
    // Not a constructor: a TypeError, as `new` throws.
    let Value::Object(arrow) = context.eval("() => 1").unwrap() else {
        panic!("an arrow function");
    };
    let arrow = runtime.as_callable(&arrow).unwrap().unwrap();
    assert!(!runtime.is_constructor(&arrow).unwrap());
    let result = context.construct(&arrow, &[]);
    assert!(thrown(&context, result).starts_with("TypeError"));
    // Descriptors.
    let object = context.new_object().unwrap();
    let key = runtime.intern_property_key("k").unwrap();
    assert!(
        context
            .define_own_property(
                &object,
                &key,
                &OrdinaryPropertyDescriptor::data(Value::Int(1), false, true, false)
            )
            .unwrap()
    );
    assert_eq!(
        context.get_own_property(&object, &key).unwrap(),
        Some(CompleteOrdinaryPropertyDescriptor::Data {
            value: Value::Int(1),
            writable: false,
            enumerable: true,
            configurable: false,
        })
    );
    assert!(!runtime.delete_property(&object, &key).unwrap());
    let getter = match context.eval("(function () { return 9; })").unwrap() {
        Value::Object(getter) => runtime.as_callable(&getter).unwrap().unwrap(),
        _ => panic!("a function"),
    };
    let accessor = runtime.intern_property_key("a").unwrap();
    assert!(
        context
            .define_own_property(
                &object,
                &accessor,
                &OrdinaryPropertyDescriptor {
                    get: DescriptorField::Present(AccessorValue::Callable(getter.clone())),
                    configurable: DescriptorField::Present(true),
                    ..OrdinaryPropertyDescriptor::new()
                },
            )
            .unwrap()
    );
    assert_eq!(
        context.get_property(&object, &accessor).unwrap(),
        Value::Int(9)
    );
    assert!(matches!(
        context.get_own_property(&object, &accessor).unwrap(),
        Some(CompleteOrdinaryPropertyDescriptor::Accessor {
            get: Some(_),
            set: None,
            ..
        })
    ));
    let keys = runtime.own_property_keys(&object).unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(
        runtime.property_key_to_js_string(&keys[0]).unwrap(),
        JsString::from_static("k")
    );
    assert!(runtime.has_own_property(&object, &accessor).unwrap());
    assert!(runtime.delete_property(&object, &accessor).unwrap());
    // Arrays and indices.
    let array = context
        .new_array_from_values(vec![Value::Int(1), Value::Int(2)])
        .unwrap();
    assert!(runtime.is_array_object(&array).unwrap());
    let length = runtime.intern_property_key("length").unwrap();
    assert_eq!(
        context.get_property(&array, &length).unwrap(),
        Value::Int(2)
    );
    let one = runtime.intern_property_key("1").unwrap();
    assert_eq!(context.get_property(&array, &one).unwrap(), Value::Int(2));
}

#[test]
fn symbols_and_conversions() {
    let (runtime, context) = runtime();
    let symbol = runtime
        .new_symbol(Some(JsString::from_static("tag")))
        .unwrap();
    assert_eq!(
        runtime.symbol_description(&symbol).unwrap(),
        Some(JsString::from_static("tag"))
    );
    let iterator = runtime
        .well_known_symbol(WellKnownSymbol::Iterator)
        .unwrap();
    assert_eq!(
        context.eval("Symbol.iterator").unwrap(),
        Value::Symbol(iterator.clone())
    );
    let array = context.new_array().unwrap();
    assert!(matches!(
        context.get_property(&array, &iterator.into()).unwrap(),
        Value::Object(_)
    ));
    assert_eq!(
        context.to_js_string(&Value::Float(1.5)).unwrap(),
        JsString::from_static("1.5")
    );
    assert_eq!(
        context
            .to_number(&Value::String(JsString::from_static(" 12 ")))
            .unwrap(),
        12.0
    );
    assert_eq!(
        runtime
            .type_of(&context.eval("(function(){})").unwrap())
            .unwrap(),
        "function"
    );
    assert_eq!(
        runtime
            .print_value(
                &context
                    .eval("({a: [1, 'b'], m: new Map([[1, 2]])})")
                    .unwrap()
            )
            .unwrap(),
        "{ a: [ 1, \"b\" ], m: Map(1) { 1 => 2 } }"
    );
    assert!(Value::Float(f64::NAN).same_value(&Value::Float(f64::NAN)));
    assert!(!Value::Float(0.0).same_value(&Value::Float(-0.0)));
    assert!(Value::Int(1).strict_equal(&Value::Float(1.0)));
    let error = context
        .new_native_error(NativeErrorKind::Range, "bad")
        .unwrap();
    let Value::Object(error) = error else {
        panic!("an error")
    };
    assert!(runtime.is_error_object(&error).unwrap());
}

#[test]
fn jobs_promises_and_rejections() {
    let (runtime, context) = runtime();
    runtime.set_track_rejections(true).unwrap();
    let Value::Object(promise) = context.eval("var log = []; Promise.resolve(1).then(v => log.push(v)); Promise.reject(new Error('lost')); Promise.resolve(2)").unwrap() else {
        panic!("a promise");
    };
    assert!(runtime.is_job_pending().unwrap());
    let snapshot = runtime.promise_snapshot(&promise).unwrap().unwrap();
    assert_eq!(snapshot.state(), PromiseState::Fulfilled);
    assert_eq!(snapshot.result(), &Value::Int(2));
    assert_eq!(
        runtime.run_pending_job().unwrap(),
        PendingJobOutcome::Executed
    );
    runtime.run_jobs().unwrap();
    assert_eq!(runtime.run_pending_job().unwrap(), PendingJobOutcome::NoJob);
    assert_eq!(
        context.eval("log.join()").unwrap(),
        Value::String(JsString::from_static("1"))
    );
    let rejections = runtime.take_unhandled_rejections().unwrap();
    assert_eq!(rejections.len(), 1);
    assert!(
        runtime
            .print_value(&rejections[0].1)
            .unwrap()
            .starts_with("Error: lost")
    );
    // A handler attached later removes the rejection.
    context.eval("var p = Promise.reject(1)").unwrap();
    context.eval("p.catch(() => {})").unwrap();
    runtime.run_jobs().unwrap();
    assert!(runtime.take_unhandled_rejections().unwrap().is_empty());
    // A reaction that throws rejects its derived promise; the drain goes on.
    context
        .eval("Promise.resolve().then(() => { throw 0; })")
        .unwrap();
    runtime.run_jobs().unwrap();
    assert_eq!(runtime.take_unhandled_rejections().unwrap().len(), 1);
}

#[derive(Default)]
struct Recorder {
    output: Rc<RefCell<String>>,
    events: Rc<RefCell<Vec<RejectionEvent>>>,
}

impl Host for Recorder {
    fn write(&mut self, _: Stream, text: &[u8]) {
        self.output
            .borrow_mut()
            .push_str(&String::from_utf8_lossy(text));
    }

    fn now_ms(&mut self) -> f64 {
        86_400_000.0
    }

    fn random_seed(&mut self) -> u64 {
        1
    }

    fn promise_rejection(&mut self, event: RejectionEvent, _: CoreValue) {
        self.events.borrow_mut().push(event);
    }
}

#[test]
fn the_host_receives_output_time_and_rejections() {
    let host = Recorder::default();
    let output = Rc::clone(&host.output);
    let events = Rc::clone(&host.events);
    let runtime = Runtime::with_host(host, RuntimeOptions::default());
    let context = runtime.context();
    context.install_qjs_helpers(&QjsHelpers::default()).unwrap();
    context
        .eval("print('hi', {a: 1}); console.log(Date.now())")
        .unwrap();
    assert_eq!(*output.borrow(), "hi { a: 1 }\n86400000\n");
    context
        .eval("var q = Promise.reject(0); q.then(undefined, () => {})")
        .unwrap();
    assert_eq!(
        *events.borrow(),
        [RejectionEvent::Rejected, RejectionEvent::Handled]
    );
    // The seed comes from the host: the same sequence twice.
    let first = Runtime::with_host(Recorder::default(), RuntimeOptions::default())
        .context()
        .eval("Math.random()")
        .unwrap();
    let second = Runtime::with_host(Recorder::default(), RuntimeOptions::default())
        .context()
        .eval("Math.random()")
        .unwrap();
    assert_eq!(first, second);
}

struct Files;

impl ModuleLoader for Files {
    fn load(
        &mut self,
        scope: &mut ModuleScope<'_>,
        name: &str,
        attributes: &ModuleImportAttributes,
    ) -> Result<ModuleSource, Value> {
        let json = attributes.effective().is_some();
        match name {
            "dep.js" => Ok(ModuleSource::source("export const answer = 42;")),
            "meta.js" => Ok(ModuleSource::Source {
                text: b"export const url = import.meta.url;".to_vec(),
                import_meta: vec![(
                    "url".into(),
                    Value::String(JsString::from_static("file:///meta.js")),
                )],
            }),
            "data.json" if json => Ok(ModuleSource::Json(b"{\"n\": 1}".to_vec())),
            "data.json5" => Ok(ModuleSource::Json5(
                b"{n: +2, /* c */ 'm': [0x10,],}".to_vec(),
            )),
            "compiled.js" => scope
                .compile_module("compiled.js", b"export default 'via scope';", Vec::new())
                .map(ModuleSource::Compiled),
            "broken.js" => scope
                .compile_module("broken.js", b"export let = ;", Vec::new())
                .map(ModuleSource::Compiled),
            _ => Err(scope
                .new_native_error(
                    NativeErrorKind::Reference,
                    &format!("could not load module filename '{name}'"),
                )
                .unwrap()),
        }
    }
}

fn run_module(context: &mut Context, source: &str) -> Result<Value, String> {
    let runtime = context.runtime().clone();
    let module = context
        .compile_module_bytes_with_filename(source.as_bytes(), "main.js")
        .map_err(|error| thrown::<()>(context, Err(error)))?;
    let promise = match context.execute_module(&module) {
        Ok(Value::Object(promise)) => promise,
        Ok(other) => panic!("not a promise: {other:?}"),
        Err(error) => return Err(thrown::<()>(context, Err(error))),
    };
    runtime.run_jobs().unwrap();
    let snapshot = runtime.promise_snapshot(&promise).unwrap().unwrap();
    match snapshot.state() {
        PromiseState::Fulfilled => Ok(snapshot.result().clone()),
        PromiseState::Rejected => Err(runtime.print_value(snapshot.result()).unwrap()),
        PromiseState::Pending => Err("pending".into()),
    }
}

#[test]
fn modules_load_through_the_loader_and_its_scope() {
    let (runtime, mut context) = runtime();
    runtime.set_module_loader(Files).unwrap();
    run_module(
        &mut context,
        "import { answer } from './dep.js'; import { url } from 'meta.js'; \
         import data from 'data.json' with { type: 'json' }; import five from 'data.json5'; \
         import compiled from 'compiled.js'; \
         globalThis.result = [answer, url, data.n, five.n, five.m[0], compiled].join();",
    )
    .unwrap();
    assert_eq!(
        context.eval("result").unwrap(),
        Value::String(JsString::from_static("42,file:///meta.js,1,2,16,via scope"))
    );
    // The loader's error is thrown as it made it.
    let error = run_module(&mut context, "import 'missing.js';").unwrap_err();
    assert!(
        error.starts_with("ReferenceError: could not load module filename 'missing.js'"),
        "{error}"
    );
    let error = run_module(&mut context, "import 'broken.js';").unwrap_err();
    assert!(error.starts_with("SyntaxError"), "{error}");
    // `import()` goes through the same loader.
    context
        .eval("import('./dep.js').then(m => globalThis.dynamic = m.answer)")
        .unwrap();
    runtime.run_jobs().unwrap();
    assert_eq!(context.eval("dynamic").unwrap(), Value::Int(42));
}

struct Reentrant(Runtime);

impl ModuleLoader for Reentrant {
    fn load(
        &mut self,
        scope: &mut ModuleScope<'_>,
        _: &str,
        _: &ModuleImportAttributes,
    ) -> Result<ModuleSource, Value> {
        // Re-entering the API without the scope is an error, not a panic.
        assert_eq!(self.0.context().eval("1"), Err(RuntimeError::Reentered));
        // A value cloned and dropped inside the callback.
        let object = Value::Object(scope.new_object().unwrap());
        let copy = object.clone();
        drop(object);
        drop(copy);
        Ok(ModuleSource::source("export default 1;"))
    }
}

#[test]
fn reentry_from_a_callback_is_an_error_and_values_drop_later() {
    let (runtime, mut context) = runtime();
    runtime
        .set_module_loader(Reentrant(runtime.clone()))
        .unwrap();
    assert_eq!(
        run_module(&mut context, "import x from 'x.js'; globalThis.x = x;").unwrap(),
        Value::Undefined
    );
    assert_eq!(context.eval("x").unwrap(), Value::Int(1));
}

#[test]
#[cfg_attr(
    feature = "gc-stress",
    ignore = "a collection at every allocation makes filling 8 MiB take minutes"
)]
fn a_heap_cap_surfaces_as_out_of_memory() {
    let runtime = Runtime::with_options(RuntimeOptions {
        heap: HeapLimits { max_bytes: 8 << 20 },
        ..RuntimeOptions::default()
    });
    let context = runtime.context();
    assert_eq!(
        context.eval("var keep = []; for (;;) keep.push({ a: keep.length });"),
        Err(RuntimeError::OutOfMemory)
    );
}

/// A compile that runs out of heap while it lowers the code fails with the
/// cap's `OutOfMemory`, as one that runs out while it parses does, not as an
/// internal error: an embedder that frees memory and tries again tells them
/// apart by it.
#[test]
#[cfg_attr(
    feature = "gc-stress",
    ignore = "a collection at every allocation makes filling 8 MiB take minutes"
)]
fn a_compile_past_the_heap_cap_is_out_of_memory() {
    let runtime = Runtime::with_options(RuntimeOptions {
        heap: HeapLimits { max_bytes: 8 << 20 },
        ..RuntimeOptions::default()
    });
    let context = runtime.context();
    assert_eq!(
        context.eval("var keep = []; for (;;) keep.push({ a: keep.length });"),
        Err(RuntimeError::OutOfMemory)
    );
    // The fill stops at the first arena growth past the cap, which can leave
    // a few MiB of the 8 free (2.4 MiB once the builtin natives' `length`
    // and `name` were made pending): a compile of this size needs about
    // 4.8 MiB, more than such a remainder.
    let mut source = String::from("(function () { var out = 0;\n");
    for line in 0..16000 {
        source.push_str(&format!("out += {line} * 2 + (out > {line} ? 1 : 0);\n"));
    }
    source.push_str("return out; })");
    let compiled = context.compile_with_options(
        &source,
        &quickjs_oxide::engine::api::core::CompileOptions::new("big.js").transient(),
    );
    assert!(
        matches!(compiled, Err(RuntimeError::OutOfMemory)),
        "{:?}",
        compiled.err()
    );
}

#[test]
fn memory_report_counts_a_known_graph() {
    let (runtime, context) = runtime();
    runtime.run_gc().unwrap();
    let before = runtime.memory_report().unwrap();
    context
        .eval("var graph = []; for (var i = 0; i < 1000; i++) graph.push({});")
        .unwrap();
    runtime.run_gc().unwrap();
    let after = runtime.memory_report().unwrap();
    assert_eq!(after.objects.live - before.objects.live, 1001);
    context.eval("graph = null").unwrap();
    runtime.run_gc().unwrap();
    let freed = runtime.memory_report().unwrap();
    assert_eq!(freed.objects.live, before.objects.live);
    assert!(freed.reserved_bytes() > 0);
    assert_eq!(freed.cap, usize::MAX);
    assert!(freed.code.units > 0);
    let text = freed.to_string();
    assert!(text.contains("objects"), "{text}");
}

/// The caches of units a collection frees go back: a collection with no
/// script running moves the live dynamic units' caches down and gives back
/// the free end and the room, and a moved unit's caches still serve it.
#[test]
fn a_collection_gives_back_the_caches_of_units_that_went() {
    let (runtime, context) = runtime();
    let report = || runtime.memory_report().unwrap();
    let start = report();
    // 400 run-time units, each with five property caches, and one kept
    // after them.
    context
        .eval(
            "for (let i = 0; i < 400; i++) (0, eval)('(function (o) { return o.a' + i + ' + o.b + o.c + o.d + o.e; })')({}); \
             var keep = (0, eval)('(function (o) { return o.x + o.y; })'); keep({ x: 1, y: 2 });",
        )
        .unwrap();
    let busy = report();
    // A collection at every allocation frees the units as the loop goes.
    let stress = cfg!(feature = "gc-stress");
    assert!(
        stress || busy.ic_entries >= start.ic_entries + 2000,
        "{} -> {}",
        start.ic_entries,
        busy.ic_entries
    );
    runtime.run_gc().unwrap();
    let after = report();
    assert!(
        after.ic_entries < start.ic_entries + 200,
        "entries {} -> {} -> {}, free {}",
        start.ic_entries,
        busy.ic_entries,
        after.ic_entries,
        after.ic_free
    );
    assert!(
        stress || after.table_bytes + 16 * 1000 < busy.table_bytes,
        "table bytes {} -> {}",
        busy.table_bytes,
        after.table_bytes
    );
    for _ in 0..3 {
        assert_eq!(context.eval("keep({ x: 3, y: 4 })").unwrap(), Value::Int(7));
    }
    assert_eq!(context.eval("keep({ y: 1, x: 5 })").unwrap(), Value::Int(6));
}

/// The shared-atoms probe classes every live atom once: a fresh runtime's
/// are its builtins'; code and a host's keys add theirs.
#[test]
fn the_atom_census_classes_each_atom_once() {
    let (runtime, context) = runtime();
    let fresh = runtime.atom_census().unwrap();
    assert_eq!(fresh.total, fresh.builtin);
    // The builtins' names are the process's shared atoms.
    assert_eq!(fresh.shared, fresh.total);
    context
        .eval(
            "var census = (0, eval)('(function (o) { return o.censusKey + o.length; })'); \
             var made = JSON.parse('{\"hostKey1\": 1, \"hostKey2\": 2}'); census(made);",
        )
        .unwrap();
    let busy = runtime.atom_census().unwrap();
    assert_eq!(
        busy.total,
        busy.builtin
            + busy.image_code
            + busy.static_code
            + busy.dynamic_code
            + busy.constants
            + busy.object_keys
            + busy.other,
        "{busy}"
    );
    assert!(busy.static_code >= 2, "census, made: {busy}");
    assert!(busy.dynamic_code >= 1, "censusKey: {busy}");
    assert!(busy.object_keys >= 2, "hostKey1, hostKey2: {busy}");
}

#[test]
fn symbol_rope_description_survives_collection_without_other_string_roots() {
    let (runtime, context) = runtime();
    context.eval("var retainedSymbol = (() => { let a = 'a'.repeat(256); let b = 'b'.repeat(256); return Symbol(a + b); })();").unwrap();
    runtime.run_gc().unwrap();
    // Read before allocating replacement strings in JS: reusing the freed slots
    // for identical contents could hide premature collection of the rope halves.
    let result = context.eval("retainedSymbol.description").unwrap();
    assert_eq!(
        text(&result),
        format!("{}{}", "a".repeat(256), "b".repeat(256))
    );
}

#[test]
fn one_character_strings_agree_on_every_path_and_survive_collection() {
    let (runtime, context) = runtime();
    context
        .eval(
            "var s = 'ab\\u00e9\\u03b1'; var parts = [s.charAt(0), s.at(1), s[2], s[3], \
             String.fromCharCode(98), s.split('')[2], JSON.parse('[\"a\", \"\"]')[0], \
             s.match(/(b)/)[1], s.slice(3), s.substring(1, 2), String(7), '' + 0, \
             JSON.parse('\"\\u00e9\"')];",
        )
        .unwrap();
    runtime.run_gc().unwrap();
    runtime.run_gc().unwrap();
    let result = context
        .eval(
            "[parts.join(','), parts[0] === 'a', parts[1] === parts[4], parts[2] === parts[5], \
             parts[3] === '\\u03b1', s.split('').join('') === s].join(';')",
        )
        .unwrap();
    assert_eq!(
        text(&result),
        "a,b,\u{e9},\u{3b1},b,\u{e9},a,b,\u{3b1},b,7,0,\u{e9};true;true;true;true;true"
    );
}

#[test]
fn execution_turns_and_the_stats_entries() {
    let (runtime, context) = runtime();
    context.eval("Promise.resolve().then(() => {})").unwrap();
    let inside = runtime.with_execution_turn(|| runtime.run_pending_job());
    assert!(matches!(inside, Err(RuntimeError::Invariant(_))));
    assert_eq!(
        runtime.run_pending_job().unwrap(),
        PendingJobOutcome::Executed
    );
    assert!(runtime.gc_report().unwrap().starts_with("gc: "));
    let disassembly = runtime
        .disassemble(b"function f(a) { return a + 1; }", "d.js")
        .unwrap();
    assert!(disassembly.contains("AddI"), "{disassembly}");
    assert_eq!(
        runtime.run_gc().unwrap().collections,
        runtime.gc_stats().unwrap().collections
    );
}

#[test]
#[cfg_attr(
    feature = "gc-stress",
    ignore = "gc-stress collects at every allocation"
)]
fn a_manual_gc_policy_collects_only_when_asked() {
    let (runtime, context) = runtime();
    runtime.set_gc_policy(GcPolicy::Manual).unwrap();
    assert_eq!(runtime.gc_policy().unwrap(), GcPolicy::Manual);
    let before = runtime.gc_stats().unwrap().collections;
    context
        .eval("for (var i = 0; i < 200000; i++) ({ i });")
        .unwrap();
    assert_eq!(runtime.gc_stats().unwrap().collections, before);
    runtime.set_gc_policy(GcPolicy::Automatic).unwrap();
    context
        .eval("for (var i = 0; i < 200000; i++) ({ i });")
        .unwrap();
    assert!(runtime.gc_stats().unwrap().collections > before);
}

// ---- host functions ----

use quickjs_oxide::engine::api::core::{FunctionOptions, Js, JsResult, Scope};

fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.to_utf8_lossy(),
        other => panic!("not a string: {other:?}"),
    }
}

fn string(text: &str) -> Value {
    Value::String(JsString::try_from_utf8(text).unwrap())
}

#[test]
fn a_host_function_on_the_global_object_reads_its_call() {
    let (runtime, context) = runtime();
    let global = context.global_object().unwrap();
    context
        .define_function(&global, "describe", 2, |scope: &mut Scope<'_>| {
            let mut parts = vec![scope.argc().to_string()];
            for value in scope.args() {
                parts.push(scope.type_of(&value)?);
            }
            Ok(string(&parts.join(",")))
        })
        .unwrap();
    assert_eq!(
        text(
            &context
                .eval("describe(undefined, null, true, 1, 1.5, 2n, 's', Symbol(), {}, () => 0)")
                .unwrap()
        ),
        "10,undefined,object,boolean,number,number,bigint,string,symbol,object,function"
    );
    // Fewer arguments than `length`, and the property's attributes.
    assert_eq!(text(&context.eval("describe()").unwrap()), "0");
    assert_eq!(
        text(
            &context
                .eval(
                    "var d = Object.getOwnPropertyDescriptor(globalThis, 'describe'); \
                     [d.writable, d.enumerable, d.configurable, describe.name, describe.length].join()"
                )
                .unwrap()
        ),
        "true,false,true,describe,2"
    );
    let _ = runtime;
}

#[test]
fn a_method_reads_its_receiver_and_captured_values() {
    let (_, context) = runtime();
    let counter = context
        .new_function(
            FunctionOptions::new("next").data(vec![Value::Int(0)]),
            |scope: &mut Scope<'_>| {
                let Value::Int(n) = scope.data(0) else {
                    panic!("the captured count");
                };
                scope.set_data(0, Value::Int(n + 1))?;
                let this = scope.this();
                let key = PropertyKeyText::key(scope, "base")?;
                let base = match this {
                    Value::Object(object) => scope.get_property(&object, &key)?,
                    _ => Value::Undefined,
                };
                let base = scope.to_number(&base)?;
                Ok(Value::number(base + f64::from(n + 1)))
            },
        )
        .unwrap();
    let object = context.new_object().unwrap();
    let key = context.runtime().intern_property_key("next").unwrap();
    context
        .set_property(&object, &key, Value::from(counter))
        .unwrap();
    let global = context.global_object().unwrap();
    let key = context.runtime().intern_property_key("o").unwrap();
    context
        .set_property(&global, &key, Value::Object(object))
        .unwrap();
    assert_eq!(
        text(
            &context
                .eval("o.base = 10; [o.next(), o.next(), o.next()].join()")
                .unwrap()
        ),
        "11,12,13"
    );
}

/// A helper that reads a key through either handle.
struct PropertyKeyText;

impl PropertyKeyText {
    fn key(
        js: &mut impl Js,
        name: &str,
    ) -> JsResult<quickjs_oxide::engine::api::core::PropertyKey> {
        let _ = js.global_object()?;
        Ok(
            quickjs_oxide::engine::api::core::PropertyKey::from_js_string(JsString::try_from_utf8(
                name,
            )?),
        )
    }
}

/// Code generic over where it runs: a `Context` or a host function's
/// `Scope`.
fn make_point(js: &mut impl Js, x: f64) -> JsResult<quickjs_oxide::engine::api::core::ObjectRef> {
    let point = js.new_object()?;
    let key = PropertyKeyText::key(js, "x")?;
    js.set_property(&point, &key, Value::number(x))?;
    Ok(point)
}

#[test]
fn code_generic_over_js_runs_on_both_handles() {
    let (_, mut context) = runtime();
    let point = make_point(&mut context, 1.0).unwrap();
    let global = context.global_object().unwrap();
    let key = context.runtime().intern_property_key("p").unwrap();
    context
        .set_property(&global, &key, Value::Object(point))
        .unwrap();
    context
        .define_function(&global, "point", 1, |scope: &mut Scope<'_>| {
            let x = scope.arg(0);
            let x = scope.to_number(&x)?;
            Ok(Value::Object(make_point(scope, x)?))
        })
        .unwrap();
    assert_eq!(context.eval("p.x + point(2).x").unwrap(), Value::Int(3));
}

#[test]
fn errors_cross_the_boundary_with_their_values() {
    let (_, context) = runtime();
    let global = context.global_object().unwrap();
    // A script closure's throw propagates through `?` to the script's catch.
    context
        .define_function(&global, "invoke", 1, |scope: &mut Scope<'_>| {
            let Value::Object(function) = scope.arg(0) else {
                return Err(scope.throw_error(NativeErrorKind::Type, "not a function"));
            };
            let function = scope.as_callable(&function)?.expect("callable");
            scope.call(&function, Value::Undefined, &[])
        })
        .unwrap();
    assert_eq!(
        context
            .eval(
                "var thrown = {}; \
                 try { invoke(function () { throw thrown; }); } catch (e) { e === thrown }"
            )
            .unwrap(),
        Value::Bool(true)
    );
    // An error thrown from Rust, with a stack naming the calling line.
    context
        .define_function(&global, "require_x", 1, |scope: &mut Scope<'_>| {
            Err(scope.throw_error(NativeErrorKind::Type, "x is required"))
        })
        .unwrap();
    let result = context
        .eval_with_filename(
            "\n\ntry { require_x(); } catch (e) { [e instanceof TypeError, e.message, e.stack].join('|') }",
            "caller.js",
        )
        .unwrap();
    let result = text(&result);
    assert!(
        result.starts_with(
            "true|x is required|    at require_x (native)\n    at <eval> (caller.js:3"
        ),
        "{result}"
    );
    // Engine errors become InternalErrors the script can catch.
    context
        .define_function(&global, "broken", 0, |_: &mut Scope<'_>| {
            Err(RuntimeError::Invariant("a broken host"))
        })
        .unwrap();
    assert_eq!(
        text(
            &context
                .eval("try { broken() } catch (e) { e.name + ': ' + e.message }")
                .unwrap()
        ),
        "InternalError: invariant violated: a broken host"
    );
}

/// Each level is a host call and a script call nested on the native stack,
/// as deep as a builtin's re-entry: the test declares a stack that holds a
/// thousand of them in a debug build.
#[test]
fn rust_and_javascript_recurse_through_each_other() {
    const STACK: usize = 256 << 20;
    let depth = std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(|| {
            quickjs_oxide::engine::api::declare_thread_stack(STACK);
            let (_, context) = runtime();
            let global = context.global_object().unwrap();
            context
                .define_function(&global, "down", 1, |scope: &mut Scope<'_>| {
                    let n = scope.arg(0);
                    let n = scope.to_number(&n)?;
                    if n == 0.0 {
                        return Ok(Value::Int(0));
                    }
                    let global = scope.global_object()?;
                    let key = PropertyKeyText::key(scope, "viaScript")?;
                    let Value::Object(next) = scope.get_property(&global, &key)? else {
                        panic!("viaScript");
                    };
                    let next = scope.as_callable(&next)?.expect("callable");
                    let depth = scope.call(&next, Value::Undefined, &[Value::number(n - 1.0)])?;
                    let depth = scope.to_number(&depth)?;
                    Ok(Value::number(depth + 1.0))
                })
                .unwrap();
            context
                .eval("function viaScript(n) { return down(n); } down(1000)")
                .unwrap()
                .as_number()
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(depth, Some(1000.0));
}

#[test]
fn values_a_host_function_keeps_outlive_its_call() {
    let (_, context) = runtime();
    let kept: Rc<RefCell<Vec<Value>>> = Rc::default();
    let store = Rc::clone(&kept);
    let global = context.global_object().unwrap();
    context
        .define_function(&global, "keep", 1, move |scope: &mut Scope<'_>| {
            store.borrow_mut().push(scope.arg(0));
            Ok(Value::Undefined)
        })
        .unwrap();
    context
        .eval("keep({ tag: 'a' }); keep('text'); keep([1, 2])")
        .unwrap();
    context.runtime().run_gc().unwrap();
    let kept = kept.borrow();
    let Value::Object(first) = &kept[0] else {
        panic!("an object");
    };
    let key = context.runtime().intern_property_key("tag").unwrap();
    assert_eq!(text(&context.get_property(first, &key).unwrap()), "a");
    assert_eq!(text(&kept[1]), "text");
    assert_eq!(context.runtime().print_value(&kept[2]).unwrap(), "[ 1, 2 ]");
}

#[test]
fn a_runtime_whose_function_captured_a_handle_drops() {
    let watch = Rc::new(());
    {
        let (_, context) = runtime();
        let object = context.new_object().unwrap();
        let watched = Rc::clone(&watch);
        let global = context.global_object().unwrap();
        context
            .define_function(&global, "f", 0, move |_: &mut Scope<'_>| {
                let _ = (&object, &watched);
                Ok(Value::Undefined)
            })
            .unwrap();
        assert_eq!(Rc::strong_count(&watch), 2);
    }
    assert_eq!(Rc::strong_count(&watch), 1, "the runtime leaked");
}

#[test]
fn a_handle_used_after_its_runtime_dropped_is_an_error() {
    let (_, context) = runtime();
    let object = context.new_object().unwrap();
    drop(context);
    let (_, other) = runtime();
    assert_eq!(
        other.get_prototype_of(&object),
        Err(RuntimeError::Invariant("a value of a dropped runtime"))
    );
}

#[test]
fn a_panic_in_a_host_function_poisons_the_runtime() {
    let (runtime, context) = runtime();
    let global = context.global_object().unwrap();
    context
        .define_function(&global, "boom", 0, |_: &mut Scope<'_>| -> JsResult<Value> {
            panic!("host bug")
        })
        .unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| context.eval("boom()")));
    assert!(outcome.is_err());
    assert_eq!(context.eval("1"), Err(RuntimeError::Poisoned));
    assert_eq!(runtime.run_gc().map(drop), Err(RuntimeError::Poisoned));
    assert_eq!(
        RuntimeError::Poisoned.to_string(),
        "the runtime is poisoned: a host function panicked"
    );
}

#[test]
fn a_host_function_uses_its_scope_not_the_context() {
    let (_, context) = runtime();
    let outer = context.clone();
    let global = context.global_object().unwrap();
    context
        .define_function(&global, "nested", 0, move |scope: &mut Scope<'_>| {
            assert_eq!(outer.eval("1"), Err(RuntimeError::Reentered));
            // A module compiles and runs from a host function too.
            let module = scope.compile_module_with_filename(
                "export default 1; globalThis.ran = 1",
                "inner.mjs",
            )?;
            scope.execute_module(&module)?;
            scope.eval("6 * 7")
        })
        .unwrap();
    assert_eq!(context.eval("nested()").unwrap(), Value::Int(42));
}

#[test]
fn a_constructor_sees_new_and_new_target() {
    let (_, context) = runtime();
    let point = context
        .new_function(
            FunctionOptions::new("Point").length(1).constructor(),
            |scope: &mut Scope<'_>| {
                if !scope.is_construct() {
                    return Err(scope.throw_error(NativeErrorKind::Type, "use new"));
                }
                let target = scope.new_target();
                let callee = scope.callee();
                assert_eq!(target, Value::from(callee));
                let x = scope.arg(0);
                Ok(Value::Object(make_point(scope, scope_number(x))?))
            },
        )
        .unwrap();
    let global = context.global_object().unwrap();
    let key = context.runtime().intern_property_key("Point").unwrap();
    context
        .set_property(&global, &key, Value::from(point))
        .unwrap();
    assert_eq!(context.eval("new Point(5).x").unwrap(), Value::Int(5));
    assert_eq!(
        text(
            &context
                .eval("try { Point(1) } catch (e) { e.message }")
                .unwrap()
        ),
        "use new"
    );
    let plain = context
        .new_function(FunctionOptions::new("plain"), |_: &mut Scope<'_>| {
            Ok(Value::Undefined)
        })
        .unwrap();
    assert!(matches!(
        context.construct(&plain, &[]),
        Err(RuntimeError::Exception(_))
    ));
}

fn scope_number(value: Value) -> f64 {
    value.as_number().unwrap_or(f64::NAN)
}

#[derive(Debug)]
struct Counter(std::cell::Cell<i32>);

#[test]
fn host_objects_carry_the_embedders_value() {
    let (_, context) = runtime();
    let prototype = context.new_object().unwrap();
    context
        .define_function(&prototype, "bump", 0, |scope: &mut Scope<'_>| {
            let Some(counter) = scope.this_host_data::<Counter>()? else {
                return Err(scope.throw_error(NativeErrorKind::Type, "not a Counter"));
            };
            counter.0.set(counter.0.get() + 1);
            Ok(Value::Int(counter.0.get()))
        })
        .unwrap();
    let counter = context
        .new_host_object(Counter(std::cell::Cell::new(40)), Some(&prototype))
        .unwrap();
    assert!(context.host_data::<String>(&counter).unwrap().is_none());
    let global = context.global_object().unwrap();
    let key = context.runtime().intern_property_key("counter").unwrap();
    context
        .set_property(&global, &key, Value::Object(counter.clone()))
        .unwrap();
    assert_eq!(
        context.eval("counter.bump(); counter.bump()").unwrap(),
        Value::Int(42)
    );
    assert_eq!(
        text(
            &context
                .eval("try { counter.bump.call({}) } catch (e) { e.message }")
                .unwrap()
        ),
        "not a Counter"
    );
    let datum = context.host_data::<Counter>(&counter).unwrap().unwrap();
    assert_eq!(datum.0.get(), 42);
    assert!(context.host_data::<Counter>(&prototype).unwrap().is_none());
    // an argument's datum, read without a handle; anything else, or no argument, is none
    let read = context
        .new_function(FunctionOptions::new("read"), |scope: &mut Scope<'_>| {
            Ok(match scope.arg_host_data::<Counter>(0)? {
                Some(counter) => Value::Int(counter.0.get()),
                None => Value::Null,
            })
        })
        .unwrap();
    let key = context.runtime().intern_property_key("read").unwrap();
    context
        .set_property(&global, &key, Value::Object(read.as_object().clone()))
        .unwrap();
    assert_eq!(context.eval("read(counter)").unwrap(), Value::Int(42));
    assert_eq!(context.eval("read({})").unwrap(), Value::Null);
    assert_eq!(context.eval("read(1)").unwrap(), Value::Null);
    assert_eq!(context.eval("read()").unwrap(), Value::Null);
}

#[test]
fn a_host_object_reads_as_an_ordinary_object() {
    let (runtime, context) = runtime();
    let global = context.global_object().unwrap();
    let plain = context
        .new_host_object(Counter(std::cell::Cell::new(0)), None)
        .unwrap();
    let key = runtime.intern_property_key("plain").unwrap();
    context
        .set_property(&global, &key, Value::Object(plain.clone()))
        .unwrap();
    context.eval("plain.x = 1").unwrap();
    assert_eq!(
        runtime.print_value(&Value::Object(plain)).unwrap(),
        "Object { x: 1 }"
    );
    assert_eq!(
        text(
            &context
                .eval("typeof plain + ' ' + Object.prototype.toString.call(plain)")
                .unwrap()
        ),
        "object [object Object]"
    );
    let Value::Object(prototype) = context
        .eval("({ [Symbol.toStringTag]: 'Counter' })")
        .unwrap()
    else {
        panic!("an object");
    };
    let tagged = context
        .new_host_object(Counter(std::cell::Cell::new(0)), Some(&prototype))
        .unwrap();
    let key = runtime.intern_property_key("tagged").unwrap();
    context
        .set_property(&global, &key, Value::Object(tagged))
        .unwrap();
    assert_eq!(
        text(&context.eval("String(tagged)").unwrap()),
        "[object Counter]"
    );
}

#[test]
fn a_module_loader_makes_host_functions() {
    struct Loader;
    impl ModuleLoader for Loader {
        fn load(
            &mut self,
            scope: &mut ModuleScope<'_>,
            _: &str,
            _: &ModuleImportAttributes,
        ) -> Result<ModuleSource, Value> {
            let function = scope
                .new_function(FunctionOptions::new("seven"), |_: &mut Scope<'_>| {
                    Ok(Value::Int(7))
                })
                .map_err(|_| Value::Undefined)?;
            let holder = scope
                .new_host_object(5_u8, None)
                .map_err(|_| Value::Undefined)?;
            drop(holder);
            let _ = function;
            Ok(ModuleSource::source("export default 1;"))
        }
    }
    let (runtime, context) = runtime();
    runtime.set_module_loader(Loader).unwrap();
    let module = context
        .compile_module("import one from 'x'; globalThis.one = one;")
        .unwrap();
    context.execute_module(&module).unwrap();
    runtime.run_jobs().unwrap();
    assert_eq!(context.eval("one").unwrap(), Value::Int(1));
}

// ---- interrupts ----

/// Interrupts `runtime` from another thread after `after`.
fn interrupt_later(runtime: &Runtime, after: std::time::Duration) -> std::thread::JoinHandle<()> {
    let handle = runtime.interrupt_handle();
    std::thread::spawn(move || {
        std::thread::sleep(after);
        handle.interrupt();
    })
}

#[test]
fn a_thread_interrupts_an_endless_loop() {
    let (runtime, context) = runtime();
    let started = std::time::Instant::now();
    let thread = interrupt_later(&runtime, std::time::Duration::from_millis(50));
    assert_eq!(
        context.eval("while (true) {}"),
        Err(RuntimeError::Interrupted)
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    thread.join().unwrap();
    // The flag stays set until cleared.
    assert_eq!(context.eval("for (;;) {}"), Err(RuntimeError::Interrupted));
    runtime.interrupt_handle().clear();
    assert_eq!(context.eval("1 + 1").unwrap(), Value::Int(2));
}

#[test]
fn neither_catch_nor_finally_runs_for_an_interrupt() {
    let (runtime, context) = runtime();
    runtime.interrupt_handle().interrupt();
    assert_eq!(
        context.eval("try { while (true) {} } catch (e) { globalThis.caught = true }"),
        Err(RuntimeError::Interrupted)
    );
    assert_eq!(
        context.eval("try { do {} while (true) } finally { globalThis.ran = true }"),
        Err(RuntimeError::Interrupted)
    );
    // An async function's body is not turned into a rejection.
    assert_eq!(
        context.eval("var q = (async () => { while (true) {} })()"),
        Err(RuntimeError::Interrupted)
    );
    // A recursion without loops is interrupted at its calls.
    assert_eq!(
        context.eval("function f() { f(); } try { f() } catch (e) { globalThis.caught = true }"),
        Err(RuntimeError::Interrupted)
    );
    runtime.interrupt_handle().clear();
    assert_eq!(
        context
            .eval("[typeof caught, typeof ran, typeof q].join()")
            .unwrap(),
        string("undefined,undefined,undefined")
    );
    assert!(!runtime.is_job_pending().unwrap());
}

#[test]
fn an_interrupted_executor_leaves_no_rejected_promise() {
    let (runtime, context) = runtime();
    runtime.set_track_rejections(true).unwrap();
    runtime.interrupt_handle().interrupt();
    assert_eq!(
        context.eval("var p = new Promise(() => { while (true) {} })"),
        Err(RuntimeError::Interrupted)
    );
    runtime.interrupt_handle().clear();
    assert!(!runtime.is_job_pending().unwrap());
    assert!(runtime.take_unhandled_rejections().unwrap().is_empty());
    assert_eq!(context.eval("typeof p").unwrap(), string("undefined"));
}

#[test]
fn an_interrupted_job_leaves_the_next_runnable() {
    let (runtime, context) = runtime();
    context
        .eval(
            "Promise.resolve().then(() => { while (true) {} }); \
             Promise.resolve().then(() => { globalThis.second = 2 });",
        )
        .unwrap();
    runtime.interrupt_handle().interrupt();
    assert_eq!(runtime.run_jobs(), Err(RuntimeError::Interrupted));
    runtime.interrupt_handle().clear();
    assert!(runtime.is_job_pending().unwrap());
    runtime.run_jobs().unwrap();
    assert_eq!(context.eval("second").unwrap(), Value::Int(2));
}

#[test]
fn a_host_hook_sets_a_deadline() {
    struct Deadline(std::time::Instant);
    impl Host for Deadline {
        fn interrupt(&mut self) -> bool {
            std::time::Instant::now() >= self.0
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(30);
    let runtime = Runtime::with_host(Deadline(deadline), RuntimeOptions::default());
    let context = runtime.context();
    assert_eq!(
        context.eval("var n = 0; while (true) { n++; }"),
        Err(RuntimeError::Interrupted)
    );
    assert!(std::time::Instant::now() >= deadline);
}

#[test]
fn a_backtracking_regexp_stops_at_an_interrupt() {
    // (a+)+$ on a long run of a's that does not end the input backtracks
    // exponentially: only the matcher's own polling stops it.
    struct Deadline(std::time::Instant);
    impl Host for Deadline {
        fn interrupt(&mut self) -> bool {
            std::time::Instant::now() >= self.0
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
    let timed = Runtime::with_host(Deadline(deadline), RuntimeOptions::default());
    let timed_context = timed.context();
    let started = std::time::Instant::now();
    assert_eq!(
        timed_context.eval(
            "try { /(a+)+$/.test('a'.repeat(40) + '!') } catch (e) { globalThis.caught = 1 }"
        ),
        Err(RuntimeError::Interrupted)
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    // Uncatchable, as the loop's interrupt is.
    assert_eq!(
        timed_context.eval("typeof caught").unwrap(),
        string("undefined")
    );
    // A two-byte subject, through replace, and from another thread.
    let (runtime, context) = self::runtime();
    let thread = interrupt_later(&runtime, std::time::Duration::from_millis(50));
    assert_eq!(
        context.eval("('ж' + 'a'.repeat(40) + '!').replace(/(a+)+$/, () => 'x')"),
        Err(RuntimeError::Interrupted)
    );
    thread.join().unwrap();
    runtime.interrupt_handle().clear();
    assert_eq!(
        context.eval("/(a+)+$/.test('aaa')").unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn a_counted_regexp_loop_stops_at_an_interrupt() {
    // No backtracking: counted loops of empty bodies, alone and before a
    // failing character at every start position.
    struct Deadline(std::time::Instant);
    impl Host for Deadline {
        fn interrupt(&mut self) -> bool {
            std::time::Instant::now() >= self.0
        }
    }
    for source in [
        "/(?:(?:){2147483646}){2147483646}/.test('')",
        "/(?:){2147483646}x/.test('a'.repeat(1000))",
        "/(?:\\b){2147483646}x/.test('a'.repeat(1000))",
        // Each attempt fails forward, 20,000 characters in, at every start.
        "new RegExp('a'.repeat(20000) + 'b').test('a'.repeat(1 << 22))",
        // A /g loop of matches, each inside its search's first chunk.
        "('a'.repeat(24000) + 'b').repeat(1000).match(new RegExp('a'.repeat(20000) + 'b', 'g'))",
        "('a'.repeat(24000) + 'b').repeat(1000).replace(new RegExp('a'.repeat(20000) + 'b', 'g'), () => 'x')",
    ] {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(30);
        let runtime = Runtime::with_host(Deadline(deadline), RuntimeOptions::default());
        let context = runtime.context();
        let started = std::time::Instant::now();
        assert_eq!(
            context.eval(source),
            Err(RuntimeError::Interrupted),
            "{source}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{source}"
        );
    }
}

#[test]
fn an_interrupt_unwinds_through_a_host_function() {
    let (runtime, context) = runtime();
    let global = context.global_object().unwrap();
    let seen = Rc::new(RefCell::new(None));
    let record = Rc::clone(&seen);
    context
        .define_function(&global, "spin", 1, move |scope: &mut Scope<'_>| {
            let Value::Object(function) = scope.arg(0) else {
                panic!("a function");
            };
            let function = scope.as_callable(&function)?.expect("callable");
            let result = scope.call(&function, Value::Undefined, &[]);
            *record.borrow_mut() = Some(result.clone());
            result
        })
        .unwrap();
    runtime.interrupt_handle().interrupt();
    assert_eq!(
        context.eval("try { spin(() => { while (true) {} }) } catch (e) { globalThis.caught = 1 }"),
        Err(RuntimeError::Interrupted)
    );
    assert_eq!(*seen.borrow(), Some(Err(RuntimeError::Interrupted)));
    runtime.interrupt_handle().clear();
    assert_eq!(context.eval("typeof caught").unwrap(), string("undefined"));
}

#[test]
fn only_back_edges_poll() {
    let (runtime, _) = runtime();
    let text = runtime
        .disassemble(
            b"var i = 0; while (i < 3) { if (i) break; i++; }",
            "loop.js",
        )
        .unwrap();
    let jumps: Vec<&str> = text
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter(|op| op.starts_with("Jmp"))
        .collect();
    // The `break` jumps forward; the loop's end jumps back.
    assert_eq!(jumps, ["Jmp", "JmpBack"], "{text}");
}

// ---- promises from Rust ----

use quickjs_oxide::engine::api::core::PromiseCapability;

#[test]
fn a_host_function_returns_a_promise_the_embedder_settles() {
    let (runtime, context) = runtime();
    let pending: Rc<RefCell<Vec<PromiseCapability>>> = Rc::default();
    let keep = Rc::clone(&pending);
    let global = context.global_object().unwrap();
    context
        .define_function(&global, "later", 0, move |scope: &mut Scope<'_>| {
            let capability = scope.new_promise()?;
            let promise = capability.promise.clone();
            keep.borrow_mut().push(capability);
            Ok(Value::Object(promise))
        })
        .unwrap();
    context
        .eval(
            "(async () => { globalThis.got = await later(); })(); \
             later().catch(e => { globalThis.caught = e; }); \
             later().then(v => { globalThis.followed = v; });",
        )
        .unwrap();
    runtime.run_jobs().unwrap();
    assert_eq!(context.eval("typeof got").unwrap(), string("undefined"));
    let mut context = context;
    let capabilities = pending.borrow().clone();
    assert_eq!(
        runtime
            .promise_snapshot(&capabilities[0].promise)
            .unwrap()
            .unwrap()
            .state(),
        PromiseState::Pending
    );
    capabilities[0]
        .resolve(&mut context, Value::Int(5))
        .unwrap();
    // Settling twice is a no-op.
    capabilities[0]
        .resolve(&mut context, Value::Int(6))
        .unwrap();
    capabilities[1].reject(&mut context, string("no")).unwrap();
    let thenable = context
        .eval("({ then(resolve) { resolve('via then'); } })")
        .unwrap();
    capabilities[2].resolve(&mut context, thenable).unwrap();
    runtime.run_jobs().unwrap();
    assert_eq!(context.eval("got").unwrap(), Value::Int(5));
    assert_eq!(context.eval("caught").unwrap(), string("no"));
    assert_eq!(context.eval("followed").unwrap(), string("via then"));
    let fulfilled = runtime
        .promise_snapshot(&capabilities[0].promise)
        .unwrap()
        .unwrap();
    assert_eq!(fulfilled.state(), PromiseState::Fulfilled);
    assert_eq!(fulfilled.result(), &Value::Int(5));
    assert_eq!(
        runtime
            .promise_snapshot(&capabilities[1].promise)
            .unwrap()
            .unwrap()
            .state(),
        PromiseState::Rejected
    );
}

#[test]
fn promise_then_runs_host_handlers() {
    let (runtime, context) = runtime();
    let seen: Rc<RefCell<Vec<String>>> = Rc::default();
    let (on_value, on_error) = (Rc::clone(&seen), Rc::clone(&seen));
    let Value::Object(resolved) = context.eval("Promise.resolve(42)").unwrap() else {
        panic!("a promise");
    };
    let chained = context
        .promise_then(
            &resolved,
            move |scope: &mut Scope<'_>| {
                let value = scope.arg(0);
                on_value.borrow_mut().push(format!("value {value:?}"));
                Ok(Value::Int(1))
            },
            move |scope: &mut Scope<'_>| {
                let reason = scope.arg(0);
                on_error.borrow_mut().push(format!("reason {reason:?}"));
                Ok(Value::Undefined)
            },
        )
        .unwrap();
    let Value::Object(rejected) = context.eval("Promise.reject('bad')").unwrap() else {
        panic!("a promise");
    };
    let record = Rc::clone(&seen);
    context
        .promise_then(
            &rejected,
            |_: &mut Scope<'_>| Ok(Value::Undefined),
            move |scope: &mut Scope<'_>| {
                let reason = scope.arg(0);
                record.borrow_mut().push(format!("reason {reason:?}"));
                Ok(Value::Undefined)
            },
        )
        .unwrap();
    runtime.run_jobs().unwrap();
    assert_eq!(*seen.borrow(), ["value Int(42)", "reason String(\"bad\")"]);
    let chained = runtime.promise_snapshot(&chained).unwrap().unwrap();
    assert_eq!(chained.result(), &Value::Int(1));
}

#[test]
fn an_unsettled_capability_leaks_nothing() {
    let (runtime, context) = runtime();
    let capability = context.new_promise().unwrap();
    assert_eq!(
        runtime
            .promise_snapshot(&capability.promise)
            .unwrap()
            .unwrap()
            .state(),
        PromiseState::Pending
    );
    runtime.run_gc().unwrap();
    let before = runtime.memory_report().unwrap().objects.live;
    for _ in 0..100 {
        drop(context.new_promise().unwrap());
    }
    runtime.run_gc().unwrap();
    assert_eq!(runtime.memory_report().unwrap().objects.live, before);
}

// ---- native modules ----

#[test]
fn a_defined_module_is_imported_by_name() {
    let (runtime, context) = runtime();
    let add = context
        .new_function(
            FunctionOptions::new("add").length(2),
            |scope: &mut Scope<'_>| {
                let (a, b) = (scope.arg(0), scope.arg(1));
                Ok(Value::number(scope.to_number(&a)? + scope.to_number(&b)?))
            },
        )
        .unwrap();
    context
        .define_module(
            "host:math",
            vec![
                ("add".into(), Value::from(add)),
                ("pi".into(), Value::Float(3.5)),
                ("default".into(), string("the default")),
            ],
        )
        .unwrap();
    let module = context
        .compile_module(
            "import { add, pi } from 'host:math'; import d from 'host:math'; \
             import * as m from 'host:math'; \
             globalThis.result = [add(2, 3), pi, d, Object.keys(m).join()].join('|');",
        )
        .unwrap();
    context.execute_module(&module).unwrap();
    runtime.run_jobs().unwrap();
    assert_eq!(
        context.eval("result").unwrap(),
        string("5|3.5|the default|add,default,pi")
    );
    // A missing name is the usual link-time SyntaxError.
    let module = context
        .compile_module("import { nope } from 'host:math';")
        .unwrap();
    let thrown = match context.execute_module(&module) {
        Err(RuntimeError::Exception(error)) => runtime.print_value(&error).unwrap(),
        Ok(Value::Object(promise)) => {
            runtime.run_jobs().unwrap();
            let snapshot = runtime.promise_snapshot(&promise).unwrap().unwrap();
            runtime.print_value(snapshot.result()).unwrap()
        }
        other => panic!("{other:?}"),
    };
    assert!(thrown.starts_with("SyntaxError"), "{thrown}");
}

#[test]
fn a_loader_returns_a_synthetic_module() {
    struct Loader;
    impl ModuleLoader for Loader {
        fn load(
            &mut self,
            scope: &mut ModuleScope<'_>,
            name: &str,
            _: &ModuleImportAttributes,
        ) -> Result<ModuleSource, Value> {
            let name_value = Value::String(JsString::try_from_utf8(name).unwrap());
            let seven = scope
                .new_function(FunctionOptions::new("seven"), |_: &mut Scope<'_>| {
                    Ok(Value::Int(7))
                })
                .map_err(|_| Value::Undefined)?;
            Ok(ModuleSource::Synthetic {
                exports: vec![
                    ("name".into(), name_value),
                    ("seven".into(), Value::from(seven)),
                ],
            })
        }
    }
    let (runtime, context) = runtime();
    runtime.set_module_loader(Loader).unwrap();
    let module = context
        .compile_module("import { name, seven } from 'native:x'; globalThis.r = name + seven();")
        .unwrap();
    context.execute_module(&module).unwrap();
    runtime.run_jobs().unwrap();
    assert_eq!(context.eval("r").unwrap(), string("native:x7"));
    // Its namespace prints like a compiled module's.
    let module = context
        .compile_module("import * as ns from 'native:y'; globalThis.ns = ns;")
        .unwrap();
    context.execute_module(&module).unwrap();
    runtime.run_jobs().unwrap();
    context
        .compile_module_with_filename(
            "export const name = 'native:y'; export function seven() { return 7; }",
            "compiled:y",
        )
        .unwrap();
    let module = context
        .compile_module("import * as ns from 'compiled:y'; globalThis.compiled = ns;")
        .unwrap();
    context.execute_module(&module).unwrap();
    runtime.run_jobs().unwrap();
    let ns = context.eval("ns").unwrap();
    let compiled = context.eval("compiled").unwrap();
    assert_eq!(
        runtime.print_value(&ns).unwrap(),
        runtime.print_value(&compiled).unwrap()
    );
    assert_eq!(
        context
            .eval("Object.keys(ns).join() + typeof ns.seven")
            .unwrap(),
        string("name,sevenfunction")
    );
}

// ---- buffers ----

use quickjs_oxide::engine::api::core::TypedArrayKind;

fn global(context: &Context, name: &str, value: Value) {
    let global = context.global_object().unwrap();
    let key = context.runtime().intern_property_key(name).unwrap();
    context.set_property(&global, &key, value).unwrap();
}

#[test]
fn buffer_bytes_round_trip() {
    let (_, context) = runtime();
    let bytes: Vec<u8> = (0..1 << 20).map(|index| (index * 7) as u8).collect();
    let buffer = context.new_array_buffer(&bytes).unwrap();
    assert_eq!(context.array_buffer_len(&buffer).unwrap(), Some(1 << 20));
    assert_eq!(context.array_buffer_to_vec(&buffer).unwrap(), bytes);
    let sum = context
        .with_array_buffer_bytes(&buffer, |bytes| {
            bytes.iter().map(|&b| u64::from(b)).sum::<u64>()
        })
        .unwrap();
    assert_eq!(sum, bytes.iter().map(|&b| u64::from(b)).sum::<u64>());
    // A write is seen by a script's view.
    context.write_array_buffer(&buffer, 3, &[200, 201]).unwrap();
    global(&context, "buffer", Value::Object(buffer.clone()));
    assert_eq!(
        context
            .eval("var u = new Uint8Array(buffer); [u[2], u[3], u[4], u[5]].join()")
            .unwrap(),
        string("14,200,201,35")
    );
    // Offsets at and past the end.
    context.write_array_buffer(&buffer, 1 << 20, &[]).unwrap();
    let past = context.write_array_buffer(&buffer, (1 << 20) - 1, &[1, 2]);
    assert!(thrown(&context, past).starts_with("RangeError"));
    // Not a buffer.
    let object = context.new_object().unwrap();
    assert_eq!(context.array_buffer_len(&object).unwrap(), None);
    let not = context.array_buffer_to_vec(&object);
    assert!(thrown(&context, not).starts_with("TypeError"));
}

#[test]
fn a_detached_buffer_throws_quickjs_s_type_error() {
    let (_, context) = runtime();
    let buffer = context.new_array_buffer(&[1, 2, 3]).unwrap();
    context
        .detach_array_buffer(&Value::Object(buffer.clone()))
        .unwrap();
    assert_eq!(context.array_buffer_len(&buffer).unwrap(), None);
    let read = context.array_buffer_to_vec(&buffer);
    assert!(thrown(&context, read).starts_with("TypeError: ArrayBuffer is detached"),);
    let write = context.write_array_buffer(&buffer, 0, &[1]);
    assert!(thrown(&context, write).starts_with("TypeError: ArrayBuffer is detached"));
}

#[test]
fn typed_array_views_report_their_window() {
    let (_, context) = runtime();
    let Value::Object(array) = context
        .eval("new Float64Array(new ArrayBuffer(64), 16, 3)")
        .unwrap()
    else {
        panic!("a typed array");
    };
    let view = context.typed_array_view(&array).unwrap().unwrap();
    assert_eq!(view.kind, TypedArrayKind::Float64);
    assert_eq!((view.byte_offset, view.length), (16, Some(3)));
    assert_eq!(context.array_buffer_len(&view.buffer).unwrap(), Some(64));
    // A length-tracking view on a resizable buffer follows it.
    let Value::Object(tracking) = context
        .eval("var rb = new ArrayBuffer(8, { maxByteLength: 32 }); new Uint16Array(rb, 2)")
        .unwrap()
    else {
        panic!("a typed array");
    };
    assert_eq!(
        context.typed_array_view(&tracking).unwrap().unwrap().length,
        Some(3)
    );
    context.eval("rb.resize(32)").unwrap();
    assert_eq!(
        context.typed_array_view(&tracking).unwrap().unwrap().length,
        Some(15)
    );
    context.eval("rb.resize(1)").unwrap();
    assert_eq!(
        context.typed_array_view(&tracking).unwrap().unwrap().length,
        None
    );
    // A DataView is not a typed array.
    let Value::Object(data_view) = context.eval("new DataView(new ArrayBuffer(4))").unwrap() else {
        panic!("a DataView");
    };
    assert!(context.typed_array_view(&data_view).unwrap().is_none());
    // Made from Rust.
    let buffer = context.new_array_buffer(&[1, 0, 2, 0, 3, 0]).unwrap();
    let made = context
        .new_typed_array(TypedArrayKind::Uint16, &buffer, 2, Some(2))
        .unwrap();
    global(&context, "made", Value::Object(made));
    assert_eq!(context.eval("[...made].join()").unwrap(), string("2,3"));
}

#[test]
fn a_shared_buffer_round_trips() {
    let (_, context) = runtime();
    let Value::Object(shared) = context
        .eval("var s = new SharedArrayBuffer(4); new Uint8Array(s).set([9, 8, 7, 6]); s")
        .unwrap()
    else {
        panic!("a SharedArrayBuffer");
    };
    assert_eq!(context.array_buffer_to_vec(&shared).unwrap(), [9, 8, 7, 6]);
    context.write_array_buffer(&shared, 1, &[1]).unwrap();
    assert_eq!(
        context
            .with_array_buffer_bytes(&shared, <[u8]>::to_vec)
            .unwrap(),
        [9, 1, 7, 6]
    );
    assert_eq!(context.eval("new Uint8Array(s)[1]").unwrap(), Value::Int(1));
}

// ---- conversions ----

use quickjs_oxide::engine::api::core::{CallableRef, FromJs, IntoJs, ObjectRef};

#[test]
fn strings_cross_the_boundary_in_every_representation() {
    let (_, context) = runtime();
    let global = context.global_object().unwrap();
    // Each string comes in as a `JsString` and goes back out unchanged.
    context
        .define_function(&global, "echo", 1, |_: &mut Scope<'_>, s: JsString| Ok(s))
        .unwrap();
    context
        .define_function(&global, "lossy", 1, |_: &mut Scope<'_>, s: String| Ok(s))
        .unwrap();
    let cases = [
        ("ASCII", "'plain'", "plain"),
        ("Latin-1", "'caf\\u00e9'", "caf\u{e9}"),
        ("wide", "'\\u4e2d\\u6587'", "\u{4e2d}\u{6587}"),
        ("a pair", "'\\ud83d\\ude00'", "\u{1f600}"),
        ("a lone surrogate", "'a\\ud800b'", "a\u{fffd}b"),
        ("a rope", "'x'.repeat(600) + 'y'.repeat(600)", ""),
        ("empty", "''", ""),
    ];
    for (name, source, utf8) in cases {
        assert_eq!(
            context
                .eval(&format!("var s = {source}; echo(s) === s"))
                .unwrap(),
            Value::Bool(true),
            "{name}"
        );
        if name == "a rope" {
            assert_eq!(
                text(&context.eval("lossy(s)").unwrap()),
                "x".repeat(600) + &"y".repeat(600)
            );
        } else {
            assert_eq!(text(&context.eval("lossy(s)").unwrap()), utf8, "{name}");
        }
    }
    // A string the embedder made, in each representation, read by a script.
    let long = JsString::try_from_utf8(&"x".repeat(600)).unwrap();
    for (string, length) in [
        (long.try_concat(&long).unwrap(), 1200),
        (JsString::from_static("plain"), 5),
        (JsString::try_from_utf8("caf\u{e9}").unwrap(), 4),
        (JsString::try_from_utf16([0x4e2d, 0xd800]).unwrap(), 2),
    ] {
        let key = context.runtime().intern_property_key("given").unwrap();
        context
            .set_property(&global, &key, Value::String(string.clone()))
            .unwrap();
        assert_eq!(context.eval("given.length").unwrap(), Value::Int(length));
        assert_eq!(context.eval("given").unwrap(), Value::String(string));
    }
}

#[test]
fn typed_host_functions_convert_their_arguments_and_result() {
    let (_, context) = runtime();
    let global = context.global_object().unwrap();
    context
        .define_function(&global, "add", 2, |_: &mut Scope<'_>, a: f64, b: f64| {
            Ok(a + b)
        })
        .unwrap();
    assert_eq!(context.eval("add(2, '3')").unwrap(), Value::Int(5));
    // A missing argument converts `undefined`.
    assert_eq!(
        context.eval("Number.isNaN(add(1))").unwrap(),
        Value::Bool(true)
    );
    context
        .define_function(
            &global,
            "eight",
            8,
            |_: &mut Scope<'_>,
             a: i32,
             b: u32,
             c: f64,
             d: bool,
             e: String,
             f: Option<i32>,
             g: Vec<i32>,
             h: JsString| {
                Ok(format!(
                    "{a}|{b}|{c}|{d}|{e}|{f:?}|{g:?}|{}",
                    h.to_utf8_lossy()
                ))
            },
        )
        .unwrap();
    assert_eq!(
        text(
            &context
                .eval("eight(-1.9, -1, '2.5', 0, 7, undefined, [1, '2', 3.7], {})")
                .unwrap()
        ),
        "-1|4294967295|2.5|false|7|None|[1, 2, 3]|[object Object]"
    );
    // A conversion that throws is the script's error.
    assert_eq!(
        text(
            &context
                .eval("try { add(Symbol()) } catch (e) { e.message }")
                .unwrap()
        ),
        "cannot convert symbol to number"
    );
    assert_eq!(
        text(
            &context
                .eval("try { eight(0, 0, 0, 0, Symbol()) } catch (e) { e.message }")
                .unwrap()
        ),
        "cannot convert symbol to string"
    );
    assert_eq!(
        text(
            &context
                .eval("try { eight(0, 0, 0, 0, '', 0, 5) } catch (e) { e.name + ': ' + e.message }")
                .unwrap()
        ),
        "TypeError: not an object"
    );
}

#[test]
fn from_js_follows_the_specification_s_conversions() {
    let (_, mut context) = runtime();
    let js = &mut context;
    assert_eq!(i32::from_js(js, Value::Float(f64::NAN)).unwrap(), 0);
    assert_eq!(i32::from_js(js, Value::Float(4294967297.5)).unwrap(), 1);
    assert_eq!(
        i32::from_js(js, Value::Float(2147483648.0)).unwrap(),
        i32::MIN
    );
    assert_eq!(u32::from_js(js, Value::Int(-2)).unwrap(), u32::MAX - 1);
    assert_eq!(u32::from_js(js, Value::Float(f64::INFINITY)).unwrap(), 0);
    assert!(f64::from_js(js, string("x")).unwrap().is_nan());
    assert!(!bool::from_js(js, string("")).unwrap());
    assert!(bool::from_js(js, Value::Float(-1.0)).unwrap());
    // A lone surrogate becomes U+FFFD in a Rust string, and stays in a
    // JsString.
    let lone = js.eval("'a\\uD800b'").unwrap();
    assert_eq!(String::from_js(js, lone.clone()).unwrap(), "a\u{fffd}b");
    assert_eq!(
        JsString::from_js(js, lone).unwrap().utf16_units().count(),
        3
    );
    // A sparse array's holes read as undefined.
    let sparse = js.eval("[1, , 3]").unwrap();
    assert_eq!(
        Vec::<Option<i32>>::from_js(js, sparse).unwrap(),
        [Some(1), None, Some(3)]
    );
    let array_like = js.eval("({ length: 2, 0: 'a', 1: 'b' })").unwrap();
    assert_eq!(Vec::<String>::from_js(js, array_like).unwrap(), ["a", "b"]);
    assert_eq!(Option::<f64>::from_js(js, Value::Null).unwrap(), None);
    assert_eq!(
        Option::<f64>::from_js(js, Value::Int(0)).unwrap(),
        Some(0.0)
    );
    // Objects and callables.
    let object = js.eval("({})").unwrap();
    assert!(ObjectRef::from_js(js, object.clone()).is_ok());
    assert!(matches!(
        CallableRef::from_js(js, object),
        Err(RuntimeError::Exception(_))
    ));
    let function = js.eval("(function () {})").unwrap();
    assert!(CallableRef::from_js(js, function).is_ok());
    assert!(matches!(
        ObjectRef::from_js(js, Value::Int(1)),
        Err(RuntimeError::Exception(_))
    ));
    // A host datum, and a value of another type.
    let host = js
        .new_host_object(Counter(std::cell::Cell::new(3)), None)
        .unwrap();
    let counter = Rc::<Counter>::from_js(js, Value::Object(host.clone())).unwrap();
    assert_eq!(counter.0.get(), 3);
    let Err(RuntimeError::Exception(error)) = Rc::<String>::from_js(js, Value::Object(host)) else {
        panic!("a wrong host type converted");
    };
    let message = js.runtime().print_value(&error).unwrap();
    assert!(
        message.starts_with("TypeError: String object expected"),
        "{message}"
    );
}

#[test]
fn into_js_makes_javascript_values() {
    let (_, mut context) = runtime();
    let js = &mut context;
    assert_eq!(().into_js(js).unwrap(), Value::Undefined);
    assert_eq!(true.into_js(js).unwrap(), Value::Bool(true));
    assert_eq!(7_i32.into_js(js).unwrap(), Value::Int(7));
    assert_eq!(u32::MAX.into_js(js).unwrap(), Value::Float(4294967295.0));
    assert_eq!(2.0_f64.into_js(js).unwrap(), Value::Int(2));
    assert_eq!("s".into_js(js).unwrap(), string("s"));
    assert_eq!(String::from("t").into_js(js).unwrap(), string("t"));
    assert_eq!(None::<i32>.into_js(js).unwrap(), Value::Undefined);
    assert_eq!(Some(1).into_js(js).unwrap(), Value::Int(1));
    let array = vec![Some(1), None].into_js(js).unwrap();
    assert_eq!(
        js.runtime().print_value(&array).unwrap(),
        "[ 1, undefined ]"
    );
    let failed: Result<i32, RuntimeError> = Err(RuntimeError::Invariant("x"));
    assert_eq!(failed.into_js(js), Err(RuntimeError::Invariant("x")));
}

#[test]
fn call_and_construct_pass_every_argument_few_or_many() {
    let (_runtime, context) = runtime();
    let Value::Object(function) = context
        .eval("(function F() { var s = [].join.call(arguments); if (new.target) this.s = s; return s; })")
        .unwrap()
    else {
        panic!("a function")
    };
    let function = context.as_callable(&function).unwrap().unwrap();
    let key = context.runtime().intern_property_key("s").unwrap();
    for count in [0, 1, 4, 8, 9, 20] {
        let arguments: Vec<Value> = (0..count).map(Value::Int).collect();
        let expected = (0..count)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let Value::String(called) = context
            .call(&function, Value::Undefined, &arguments)
            .unwrap()
        else {
            panic!("a string")
        };
        assert_eq!(called.to_utf8_lossy(), expected, "call with {count}");
        let Value::Object(made) = context.construct(&function, &arguments).unwrap() else {
            panic!("an object")
        };
        let Value::String(constructed) = context.get_property(&made, &key).unwrap() else {
            panic!("a string")
        };
        assert_eq!(
            constructed.to_utf8_lossy(),
            expected,
            "construct with {count}"
        );
    }
}

#[test]
fn ordinary_objects_are_told_from_exotic_ones() {
    let (runtime, context) = runtime();
    let object_of = |text: &str| match context.eval(text).unwrap() {
        Value::Object(object) => object,
        other => panic!("not an object: {other:?}"),
    };
    for text in [
        "({a: 1})",
        "[1, 2]",
        "(function () {})",
        "({get a() { return 1 }})",
    ] {
        assert!(
            runtime.is_ordinary_object(&object_of(text)).unwrap(),
            "{text}"
        );
    }
    for text in ["new Proxy({}, {})", "new String('s')", "new Uint8Array(2)"] {
        assert!(
            !runtime.is_ordinary_object(&object_of(text)).unwrap(),
            "{text}"
        );
    }
}

#[test]
fn plain_objects_are_made_and_read_in_one_step() {
    use quickjs_oxide::engine::api::core::{InitValue, PropertyKey};
    let (runtime, context) = runtime();
    let key = |text: &str| PropertyKey::from_js_string(JsString::try_from_utf8(text).unwrap());
    let text = |text: &str| Value::String(JsString::try_from_utf8(text).unwrap());
    // a setter on Object.prototype does not run for a data property made in one step
    context
        .eval("Object.defineProperty(Object.prototype, 'b', {set(v) { globalThis.ran = v }, configurable: true})")
        .unwrap();
    let made = runtime
        .new_object_from(&[
            (key("a"), Value::Int(1).into()),
            (key("b"), InitValue::Text("two")),
            (key("a"), Value::Int(3).into()),
        ])
        .unwrap();
    let show = match context
        .eval("(o) => JSON.stringify(o) + ' ' + typeof globalThis.ran")
        .unwrap()
    {
        Value::Object(object) => runtime.as_callable(&object).unwrap().expect("a function"),
        other => panic!("not a function: {other:?}"),
    };
    let cx = runtime.context();
    assert_eq!(
        cx.call(&show, Value::Undefined, &[Value::Object(made.clone())])
            .unwrap(),
        text(r#"{"a":3,"b":"two"} undefined"#)
    );
    let object_prototype = runtime.get_prototype_of(&made).unwrap();
    let entries = runtime
        .plain_own_data(&made, object_prototype.as_ref())
        .unwrap()
        .expect("a plain object");
    let read: Vec<(Option<JsString>, Value, bool)> = entries
        .into_iter()
        .map(|entry| (entry.key.to_js_string(), entry.value, entry.enumerable))
        .collect();
    assert_eq!(
        read,
        vec![
            (JsString::try_from_utf8("a").ok(), Value::Int(3), true),
            (JsString::try_from_utf8("b").ok(), text("two"), true),
        ]
    );
    let object_of = |source: &str| match context.eval(source).unwrap() {
        Value::Object(object) => object,
        other => panic!("not an object: {other:?}"),
    };
    let hidden = object_of("Object.defineProperty({x: 1}, 'y', {value: 2})");
    let hidden = runtime
        .plain_own_data(&hidden, object_prototype.as_ref())
        .unwrap()
        .expect("a plain object");
    assert_eq!(
        hidden
            .iter()
            .map(|entry| entry.enumerable)
            .collect::<Vec<_>>(),
        [true, false]
    );
    for source in [
        "({get a() { return 1 }})",
        "Object.create(null)",
        "[1]",
        "new Proxy({}, {})",
        "new Map()",
        "(function () {})",
    ] {
        assert!(
            runtime
                .plain_own_data(&object_of(source), object_prototype.as_ref())
                .unwrap()
                .is_none(),
            "{source}"
        );
    }
    assert_eq!(
        runtime.dense_array_values(&object_of("[1, 'x']")).unwrap(),
        Some(vec![Value::Int(1), text("x")])
    );
    let made = runtime
        .new_array_from_inits(&[InitValue::Text("é ☃"), Value::Null.into()])
        .unwrap();
    assert_eq!(
        runtime.dense_array_values(&made).unwrap(),
        Some(vec![text("é ☃"), Value::Null])
    );
    for source in [
        "[1, , 3]",
        "Object.defineProperty([1], 0, {get() { return 2 }})",
        "({length: 0})",
        "new Proxy([], {})",
    ] {
        assert!(
            runtime
                .dense_array_values(&object_of(source))
                .unwrap()
                .is_none(),
            "{source}"
        );
    }
}

#[test]
fn an_index_key_is_read_as_a_number() {
    use quickjs_oxide::engine::api::core::PropertyKey;
    let key = |text: &str| PropertyKey::from_js_string(JsString::try_from_utf8(text).unwrap());
    assert_eq!(key("7").as_index(), Some(7));
    assert_eq!(PropertyKey::index(42).as_index(), Some(42));
    for text in ["07", "x", "", "4294967295", "-1"] {
        assert_eq!(key(text).as_index(), None, "{text}");
    }
}

#[test]
fn a_text_argument_is_read_as_rust_text() {
    use quickjs_oxide::engine::api::core::{FunctionOptions, Scope};
    let (runtime, context) = runtime();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let into = Rc::clone(&seen);
    let function = context
        .new_function(
            FunctionOptions::new("texts"),
            move |scope: &mut Scope<'_>| {
                for index in 0..scope.argc() {
                    let lifted = match scope.arg(index) {
                        Value::String(text) => Some(text.to_utf8_lossy()),
                        _ => None,
                    };
                    let text = scope.arg_text(index);
                    assert_eq!(text, lifted);
                    into.borrow_mut().push(text);
                }
                Ok(Value::Undefined)
            },
        )
        .unwrap();
    let args = match context
        .eval(r"['ascii', 'caf\u00e9', '\u2603', '\ud800', 7, 'a' + 'b'.repeat(3)]")
        .unwrap()
    {
        Value::Object(array) => runtime.dense_array_values(&array).unwrap().unwrap(),
        other => panic!("not an array: {other:?}"),
    };
    context.call(&function, Value::Undefined, &args).unwrap();
    assert_eq!(
        *seen.borrow(),
        [
            Some("ascii".to_string()),
            Some("café".to_string()),
            Some("☃".to_string()),
            Some("\u{fffd}".to_string()),
            None,
            Some("abbb".to_string()),
        ]
    );
}

#[test]
fn a_key_used_again_reads_the_same_property_across_collections() {
    use quickjs_oxide::engine::api::core::PropertyKey;
    let (runtime, context) = runtime();
    let key = PropertyKey::from_js_string(JsString::try_from_utf8("someKeyName").unwrap());
    let other = PropertyKey::from_js_string(JsString::try_from_utf8("someOtherName").unwrap());
    let cx = runtime.context();
    for round in 0..4 {
        let object = cx.new_object().unwrap();
        cx.set_property(&object, &key, Value::Int(round)).unwrap();
        cx.set_property(&object, &other, Value::Int(-round))
            .unwrap();
        // a collection may free the atoms between uses; the key interns again
        runtime.run_gc().unwrap();
        assert_eq!(cx.get_property(&object, &key).unwrap(), Value::Int(round));
        assert_eq!(
            cx.get_property(&object, &other).unwrap(),
            Value::Int(-round)
        );
        let seen = match context
            .eval("(o) => o.someKeyName * 10 + (o.someOtherName || 0)")
            .unwrap()
        {
            Value::Object(function) => runtime.as_callable(&function).unwrap().unwrap(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            context
                .call(&seen, Value::Undefined, &[Value::Object(object)])
                .unwrap(),
            Value::Int(round * 10 - round)
        );
    }
}

#[test]
fn a_string_is_compared_with_rust_text() {
    let text = |source: &str| JsString::try_from_utf8(source).unwrap();
    assert!(text("abc").eq_str("abc"));
    assert!(!text("abc").eq_str("abd"));
    assert!(!text("abc").eq_str("ab"));
    assert!(text("café").eq_str("café"));
    assert!(text("☃x").eq_str("☃x"));
    assert!(!text("caf").eq_str("café"));
    assert!(text("").eq_str(""));
}

#[test]
fn a_plain_objects_properties_are_shown_with_strings_borrowed() {
    use quickjs_oxide::engine::api::core::{DataRef, TextRef};
    let (runtime, context) = runtime();
    let object_of = |source: &str| match context.eval(source).unwrap() {
        Value::Object(object) => object,
        other => panic!("not an object: {other:?}"),
    };
    let object = object_of(
        "Object.defineProperty({a: 'ascii', b: 'caf\\u00e9', c: '\\u2603', d: 4, f() {}, o: {}}, 'h', {value: 'x'})",
    );
    let prototype = runtime.get_prototype_of(&object).unwrap();
    let mut seen = Vec::new();
    let all = runtime
        .visit_plain_own_data(&object, prototype.as_ref(), &mut |property| {
            let name = property.key.to_js_string().unwrap().to_utf8_lossy();
            let shown = match property.value {
                DataRef::Text(text) => {
                    assert!(matches!(
                        (name.as_str(), text),
                        ("a" | "b" | "h", TextRef::Latin1(_)) | ("c", TextRef::Utf16(_))
                    ));
                    assert!(text.eq_str(&text.to_string_lossy()));
                    format!("text {}", text.to_string_lossy())
                }
                DataRef::Callable => "callable".to_string(),
                DataRef::Value(Value::Object(_)) => "object".to_string(),
                DataRef::Value(value) => format!("{value:?}"),
            };
            seen.push(format!("{name} {} {shown}", property.enumerable));
            true
        })
        .unwrap();
    assert_eq!(all, Some(true));
    assert_eq!(
        seen,
        [
            "a true text ascii",
            "b true text café",
            "c true text ☃",
            "d true Int(4)",
            "f true callable",
            "o true object",
            "h false text x",
        ]
    );
    let mut count = 0;
    assert_eq!(
        runtime
            .visit_plain_own_data(&object, prototype.as_ref(), &mut |_| {
                count += 1;
                count < 2
            })
            .unwrap(),
        Some(false)
    );
    for source in ["({get a() { return 1 }})", "[1]", "Object.create(null)"] {
        assert_eq!(
            runtime
                .visit_plain_own_data(&object_of(source), prototype.as_ref(), &mut |_| true)
                .unwrap(),
            None,
            "{source}"
        );
    }
}

#[test]
fn nested_plain_objects_and_arrays_are_made_in_one_step() {
    use quickjs_oxide::engine::api::core::{InitValue, PropertyKey};
    let (runtime, context) = runtime();
    let key = |text: &str| PropertyKey::from_js_string(JsString::try_from_utf8(text).unwrap());
    let rows: Vec<InitValue<'_>> = (0..50)
        .map(|index| {
            InitValue::Object(vec![
                (key("id"), InitValue::Text("row")),
                (key("n"), Value::Int(index).into()),
                (
                    key("tags"),
                    InitValue::Array(vec![InitValue::Text("a"), InitValue::Array(vec![])]),
                ),
            ])
        })
        .collect();
    let made = runtime
        .new_object_from(&[
            (key("rows"), InitValue::Array(rows)),
            (key("x"), InitValue::Object(vec![])),
        ])
        .unwrap();
    // a collection after the call keeps what the one handle reaches
    runtime.run_gc().unwrap();
    let show = match context
        .eval("(o) => o.rows.length + ' ' + o.rows[49].n + ' ' + JSON.stringify(o.rows[3]) + ' ' + JSON.stringify(o.x)")
        .unwrap()
    {
        Value::Object(object) => runtime.as_callable(&object).unwrap().unwrap(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        context
            .call(&show, Value::Undefined, &[Value::Object(made)])
            .unwrap(),
        Value::String(
            JsString::try_from_utf8(r#"50 49 {"id":"row","n":3,"tags":["a",[]]} {}"#).unwrap()
        )
    );
}

#[test]
fn a_propertys_host_datum_is_read_with_no_handle() {
    use quickjs_oxide::engine::api::core::{FunctionOptions, PropertyKey, Scope};
    let (runtime, context) = runtime();
    let key = PropertyKey::from_js_string(JsString::try_from_utf8("$held").unwrap());
    let cx = runtime.context();
    let held = cx.new_host_object(String::from("datum"), None).unwrap();
    let holder = cx.new_object().unwrap();
    cx.set_property(&holder, &key, Value::Object(held)).unwrap();
    assert_eq!(
        cx.property_host_data::<String>(&holder, &key)
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("datum")
    );
    assert!(
        cx.property_host_data::<i32>(&holder, &key)
            .unwrap()
            .is_none()
    );
    let seen = Rc::new(RefCell::new(Vec::new()));
    let into = Rc::clone(&seen);
    let read_key = key.clone();
    let function = context
        .new_function(
            FunctionOptions::new("read"),
            move |scope: &mut Scope<'_>| {
                for index in 0..scope.argc() {
                    let found = scope.arg_property_host_data::<String>(index, &read_key)?;
                    into.borrow_mut().push(found.map(|text| (*text).clone()));
                }
                Ok(Value::Undefined)
            },
        )
        .unwrap();
    let other = cx.new_object().unwrap();
    context
        .call(
            &function,
            Value::Undefined,
            &[Value::Object(holder), Value::Object(other), Value::Int(3)],
        )
        .unwrap();
    assert_eq!(*seen.borrow(), [Some("datum".to_string()), None, None]);
}
