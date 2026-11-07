//! Code compiled inside a host function, or by Rust code it lends its
//! engine to, goes in the code store as top-level code does.

use quickjs_oxide::engine::api::core::{Context, JsResult, Runtime, Scope, Value};

fn runtime() -> (Runtime, Context) {
    let runtime = Runtime::new();
    let context = runtime.context();
    (runtime, context)
}

fn units(runtime: &Runtime) -> (usize, usize) {
    let code = runtime.memory_report().unwrap().code;
    (code.static_units, code.dynamic_units)
}

#[test]
fn a_host_function_compiles_into_the_code_store() {
    let (runtime, context) = runtime();
    let global = context.global_object().unwrap();
    context
        .define_function(&global, "compileHere", 1, |scope: &mut Scope<'_>| {
            let source = scope.arg(0);
            let source = scope.to_js_string(&source)?.to_utf8_lossy();
            let script = scope.compile(&source)?;
            scope.execute(&script)
        })
        .unwrap();
    let (before, _) = units(&runtime);
    assert_eq!(
        context
            .eval("compileHere('function made(x) { return x * 3; } made(4)') + made(5)")
            .unwrap(),
        Value::Int(27)
    );
    let (after, dynamic) = units(&runtime);
    assert_eq!(
        after,
        before + 3,
        "the eval's root and the nested script's two units"
    );
    let _ = dynamic;
    // The units outlive the host function and any collection.
    runtime.run_gc().unwrap();
    assert_eq!(context.eval("made(10)").unwrap(), Value::Int(30));
    // Transient code still goes in the heap.
    context
        .define_function(&global, "transientHere", 0, |scope: &mut Scope<'_>| {
            let options = quickjs_oxide::engine::api::core::CompileOptions::default().transient();
            let script = scope.compile_with_options("(() => 9)()", &options)?;
            scope.execute(&script)
        })
        .unwrap();
    let (before, _) = units(&runtime);
    assert_eq!(context.eval("transientHere()").unwrap(), Value::Int(9));
    assert_eq!(
        units(&runtime).0,
        before + 1,
        "only the calling eval's root"
    );
}

#[test]
fn the_store_grows_while_the_running_loop_borrows_a_unit() {
    let (runtime, context) = runtime();
    let global = context.global_object().unwrap();
    context
        .define_function(&global, "define", 1, |scope: &mut Scope<'_>| {
            let index = scope.arg(0);
            let index = scope.to_number(&index)? as i32;
            let script = scope.compile(&format!(
                "(function f{index}(a) {{ return function () {{ return a + {index}; }}; }})"
            ))?;
            scope.execute(&script)
        })
        .unwrap();
    // Hundreds of units published past several chunks while this loop, a
    // unit of the store, runs; each runs right away and again at the end.
    assert_eq!(
        context
            .eval(
                "var made = [];
                 for (var i = 0; i < 400; i++) { var f = define(i)(1); made.push(f); if (f() !== i + 1) throw i; }
                 made.reduce((sum, f) => sum + f(), 0)"
            )
            .unwrap(),
        Value::Int((1..=400).sum())
    );
    assert!(units(&runtime).0 > 800);
}

#[test]
fn a_lent_engine_loads_images_and_modules_into_the_store() {
    let (runtime, context) = runtime();
    let image = context
        .compile_image(
            "function fromImage() { return 'image'; } fromImage()",
            "image.js",
        )
        .unwrap();
    let global = context.global_object().unwrap();
    let nested = context.clone();
    context
        .define_function(&global, "service", 0, move |scope: &mut Scope<'_>| {
            scope.lend(|| -> JsResult<Value> {
                let loaded = nested.load_image(&image)?;
                let from_image = nested.execute(&loaded)?;
                let dependency =
                    nested.compile_module_with_filename("export const n = 20;", "dep.mjs")?;
                let _ = dependency;
                let module = nested.compile_module_with_filename(
                    "import { n } from 'dep.mjs'; globalThis.fromModule = n + 1;",
                    "main.mjs",
                )?;
                nested.execute_module(&module)?;
                Ok(from_image)
            })
        })
        .unwrap();
    let (before, _) = units(&runtime);
    assert_eq!(
        context.eval("service() + ' ' + fromModule").unwrap(),
        Value::String(quickjs_oxide::engine::api::core::JsString::from_static(
            "image 21"
        ))
    );
    assert!(units(&runtime).0 >= before + 5);
    runtime.run_gc().unwrap();
    assert_eq!(
        context.eval("fromImage()").unwrap(),
        Value::String(quickjs_oxide::engine::api::core::JsString::from_static(
            "image"
        ))
    );
}

/// A script of many functions, each its own unit.
fn many_functions(count: usize) -> String {
    let mut source = String::new();
    for index in 0..count {
        source.push_str(&format!("function f{index}() {{ return {index}; }}\n"));
    }
    source.push_str(&format!("f{}() + f0()", count - 1));
    source
}

/// More units than a shared store's overflow holds: a top-level compile has
/// no bound, and inside a host function the rest are transient.
#[test]
#[cfg_attr(
    feature = "gc-stress",
    ignore = "gc-stress collects at every allocation"
)]
fn compiles_past_the_overflow() {
    let count = 33_000;
    let source = many_functions(count);
    let (top, context) = runtime();
    let before = units(&top).0;
    assert_eq!(context.eval(&source).unwrap(), Value::Int(count as i32 - 1));
    assert!(units(&top).0 >= before + count, "every unit static");
    assert_eq!(units(&top).1, 0);

    let (runtime, context) = runtime();
    let global = context.global_object().unwrap();
    context
        .define_function(&global, "compileHere", 0, move |scope: &mut Scope<'_>| {
            let script = scope.compile(&source)?;
            scope.execute(&script)
        })
        .unwrap();
    let before = units(&runtime).0;
    assert_eq!(
        context.eval("compileHere()").unwrap(),
        Value::Int(count as i32 - 1)
    );
    let (after, dynamic) = units(&runtime);
    assert_eq!(after, before + 1 + 128 * 256, "the overflow filled");
    assert!(dynamic > 0, "the rest went to the heap");
    assert_eq!(context.eval("f0() + f32999()").unwrap(), Value::Int(32_999));
    // A later top-level compile is unbounded again.
    assert_eq!(context.eval("f1() + 1").unwrap(), Value::Int(2));
}

#[test]
fn a_lend_that_throws_after_publishing_keeps_its_units() {
    let (runtime, context) = runtime();
    let global = context.global_object().unwrap();
    let nested = context.clone();
    context
        .define_function(
            &global,
            "publishThenThrow",
            0,
            move |scope: &mut Scope<'_>| {
                scope.lend(|| {
                    nested.eval("function kept() { return 'kept'; } throw new Error('after')")
                })
            },
        )
        .unwrap();
    assert_eq!(
        context
            .eval("try { publishThenThrow() } catch (e) { e.message + ' ' + kept() }")
            .unwrap(),
        Value::String(quickjs_oxide::engine::api::core::JsString::from_static(
            "after kept"
        ))
    );
    runtime.run_gc().unwrap();
    assert_eq!(
        context.eval("kept()").unwrap(),
        Value::String(quickjs_oxide::engine::api::core::JsString::from_static(
            "kept"
        ))
    );
}

#[test]
fn a_lend_that_panics_after_publishing_poisons_the_runtime() {
    let (runtime, context) = runtime();
    let global = context.global_object().unwrap();
    let nested = context.clone();
    context
        .define_function(
            &global,
            "publishThenPanic",
            0,
            move |scope: &mut Scope<'_>| -> JsResult<Value> {
                scope.lend(|| {
                    nested.eval("function published() {}").unwrap();
                    panic!("after publishing")
                })
            },
        )
        .unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        context.eval("publishThenPanic()")
    }));
    assert!(outcome.is_err());
    assert_eq!(
        context.eval("1"),
        Err(quickjs_oxide::engine::api::core::RuntimeError::Poisoned)
    );
    drop(context);
    drop(runtime);
}
