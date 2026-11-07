//! Re-entry from Rust code a host function calls: while a host function
//! lends its engine (`Scope::lend`), `Context` and `Runtime` calls made by
//! code that has no scope in hand enter that engine.

use std::cell::Cell;
use std::rc::Rc;

use quickjs_oxide::engine::api::core::{
    Context, JsResult, JsString, NativeErrorKind, PropertyKey, Runtime, RuntimeError, Scope, Value,
};

fn runtime() -> (Runtime, Context) {
    let runtime = Runtime::new();
    let context = runtime.context();
    (runtime, context)
}

fn text(value: &str) -> Value {
    Value::String(JsString::try_from_utf8(value).unwrap())
}

fn string(context: &Context, source: &str) -> String {
    match context.eval(source) {
        Ok(Value::String(text)) => text.to_utf8_lossy(),
        Err(RuntimeError::Exception(value)) => {
            panic!(
                "{source}: threw {}",
                context.runtime().print_value(&value).unwrap()
            )
        }
        other => panic!("not a string: {other:?}"),
    }
}

/// Deep Rust code with a `Context` and no scope: what an embedder's service
/// layer is.
fn service(context: &Context, source: &str) -> JsResult<Value> {
    context.eval(source)
}

/// Defines `name(source)` on the global object: a host function that lends
/// its engine to `service`, which evaluates `source`.
fn define_lending(context: &Context, name: &str) {
    let global = context.global_object().unwrap();
    let nested = context.clone();
    context
        .define_function(&global, name, 1, move |scope: &mut Scope<'_>| {
            let source = scope.arg(0);
            let source = scope.to_js_string(&source)?.to_utf8_lossy();
            scope.lend(|| service(&nested, &source))
        })
        .unwrap();
}

#[test]
fn a_host_function_lends_its_engine_to_nested_eval() {
    let (runtime, context) = runtime();
    define_lending(&context, "nested");
    assert!(!runtime.is_lent());
    assert_eq!(
        context
            .eval("var outer = 20; let lexical = 1; nested('outer + lexical + 21')")
            .unwrap(),
        Value::Int(42)
    );
    // The nested script's declarations are the runtime's.
    assert_eq!(
        context
            .eval("nested('var fromNested = { n: 5 }; fromNested'); fromNested.n")
            .unwrap(),
        Value::Int(5)
    );
    // An object the nested script made outlives the lend.
    assert_eq!(
        string(&context, "var o = nested('({ deep: \"yes\" })'); o.deep"),
        "yes"
    );
    // Nested more than once, through the same host function.
    assert_eq!(
        context
            .eval("nested(\"nested('nested(\\\"6 * 7\\\")')\")")
            .unwrap(),
        Value::Int(42)
    );
    assert!(!runtime.is_lent());
    // Without a lend, a host function's Context call is refused as before.
    let global = context.global_object().unwrap();
    let plain = context.clone();
    let refused = Rc::new(Cell::new(false));
    let seen = Rc::clone(&refused);
    context
        .define_function(&global, "plain", 0, move |_: &mut Scope<'_>| {
            seen.set(plain.eval("1") == Err(RuntimeError::Reentered));
            Ok(Value::Undefined)
        })
        .unwrap();
    context.eval("plain()").unwrap();
    assert!(refused.get());
}

#[test]
fn every_context_and_runtime_operation_runs_while_lent() {
    let (runtime, context) = runtime();
    let global = context.global_object().unwrap();
    let nested = context.clone();
    context
        .define_function(&global, "work", 1, move |scope: &mut Scope<'_>| {
            let argument = scope.arg(0);
            let runtime = nested.runtime();
            scope.lend(|| {
                assert!(runtime.is_lent());
                let script = nested.compile("(function (x) { return x * 2; })")?;
                let Value::Object(double) = nested.execute(&script)? else {
                    return Err(RuntimeError::Invariant("a function"));
                };
                let double = nested.as_callable(&double)?.unwrap();
                let result = nested.call(&double, Value::Undefined, &[argument])?;
                let object = nested.new_object()?;
                let key = PropertyKey::from_js_string(JsString::from_static("result"));
                nested.set_property(&object, &key, result)?;
                assert_eq!(runtime.type_of(&Value::Object(object.clone()))?, "object");
                assert_eq!(
                    nested.compile_module("export default 1").map(drop),
                    Err(RuntimeError::Unsupported("modules inside a host function"))
                );
                // Collection stays at the top level.
                assert_eq!(runtime.run_gc().map(drop), Err(RuntimeError::Reentered));
                Ok(Value::Object(object))
            })
        })
        .unwrap();
    assert_eq!(context.eval("work(21).result").unwrap(), Value::Int(42));
    assert!(!runtime.is_lent());
}

#[test]
fn exceptions_from_a_nested_script_propagate() {
    let (_, context) = runtime();
    define_lending(&context, "nested");
    // The nested throw returns to the host function as its value, and the
    // host function returning it throws it on in the outer script.
    assert_eq!(
        string(
            &context,
            "var thrown = { tag: 'mine' };
             try { nested('throw thrown') } catch (e) { e === thrown ? e.tag : 'other' }"
        ),
        "mine"
    );
    assert_eq!(
        string(
            &context,
            "try { nested('null.x') } catch (e) { e instanceof TypeError && e.message }"
        ),
        "cannot read property 'x' of null"
    );
    // A host function may also catch it in Rust and throw its own.
    let global = context.global_object().unwrap();
    let nested = context.clone();
    context
        .define_function(&global, "wrap", 0, move |scope: &mut Scope<'_>| {
            let caught = scope.lend(|| nested.eval("undefinedName"));
            match caught {
                Err(RuntimeError::Exception(_)) => {
                    Err(scope.throw_error(NativeErrorKind::Range, "wrapped"))
                }
                other => other,
            }
        })
        .unwrap();
    assert_eq!(
        string(
            &context,
            "try { wrap() } catch (e) { e.name + ': ' + e.message }"
        ),
        "RangeError: wrapped"
    );
    // A syntax error in nested source.
    assert!(matches!(
        context.eval("nested('(')"),
        Err(RuntimeError::Exception(_))
    ));
    assert_eq!(context.eval("1 + 1").unwrap(), Value::Int(2));
}

#[test]
fn an_interrupt_in_a_nested_script_stops_the_outer_one() {
    let (runtime, context) = runtime();
    let handle = runtime.interrupt_handle();
    let global = context.global_object().unwrap();
    let nested = context.clone();
    let inner_result = Rc::new(Cell::new(None));
    let seen = Rc::clone(&inner_result);
    let interrupt = handle.clone();
    context
        .define_function(&global, "spin", 0, move |scope: &mut Scope<'_>| {
            interrupt.interrupt();
            let result = scope.lend(|| nested.eval("for (;;) {}"));
            seen.set(Some(result == Err(RuntimeError::Interrupted)));
            result
        })
        .unwrap();
    assert_eq!(
        context.eval("var caught = false; try { spin() } catch (e) { caught = true }"),
        Err(RuntimeError::Interrupted)
    );
    assert_eq!(inner_result.get(), Some(true));
    handle.clear();
    // No catch ran, and the runtime goes on.
    assert_eq!(context.eval("caught").unwrap(), Value::Bool(false));
    assert!(!runtime.is_lent());
}

#[test]
fn a_host_function_runs_pending_jobs_while_lent() {
    let (runtime, context) = runtime();
    let global = context.global_object().unwrap();
    let jobs = runtime.clone();
    context
        .define_function(&global, "drain", 0, move |scope: &mut Scope<'_>| {
            scope.lend(|| {
                let pending = jobs.is_job_pending()?;
                jobs.run_jobs()?;
                Ok(Value::Bool(pending))
            })
        })
        .unwrap();
    assert_eq!(
        string(
            &context,
            "var log = [];
             Promise.resolve(1).then(v => log.push('then ' + v)).then(() => log.push('second'));
             (async () => { await null; log.push('async'); })();
             log.push('before');
             var pending = drain();
             log.push('after');
             pending + ':' + log.join()"
        ),
        "true:before,then 1,async,second,after"
    );
    assert!(!runtime.is_job_pending().unwrap());
    // One job at a time.
    let jobs = runtime.clone();
    context
        .define_function(&global, "drainOne", 0, move |scope: &mut Scope<'_>| {
            let outcome = scope.lend(|| jobs.run_pending_job())?;
            Ok(Value::Bool(outcome.executed()))
        })
        .unwrap();
    assert_eq!(
        string(
            &context,
            "var steps = []; Promise.resolve().then(() => steps.push(1)).then(() => steps.push(2));
             [drainOne(), steps.length, drainOne(), steps.length].join()"
        ),
        "true,1,true,2"
    );
    assert_eq!(context.eval("drainOne()").unwrap(), Value::Bool(false));
}

#[test]
fn nested_scripts_allocate_and_collect() {
    let (runtime, context) = runtime();
    define_lending(&context, "nested");
    assert_eq!(
        context
            .eval(
                "var kept = [];
                 for (var i = 0; i < 5; i++)
                   kept.push(nested('var a = []; for (var j = 0; j < 400; j++) a.push({ j: j, s: \"x\" + j }); a.length'));
                 kept.reduce((x, y) => x + y)"
            )
            .unwrap(),
        Value::Int(2_000)
    );
    runtime.run_gc().unwrap();
    assert_eq!(context.eval("a[399].s").unwrap(), text("x399"));
}

#[test]
fn a_panic_while_lent_poisons_the_runtime() {
    let (runtime, context) = runtime();
    let global = context.global_object().unwrap();
    let nested = context.clone();
    context
        .define_function(&global, "boom", 0, move |scope: &mut Scope<'_>| {
            scope.lend(|| {
                nested.eval("1").unwrap();
                panic!("service bug")
            })
        })
        .unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| context.eval("boom()")));
    assert!(outcome.is_err());
    assert_eq!(context.eval("1"), Err(RuntimeError::Poisoned));
    assert!(!runtime.is_lent());
}
