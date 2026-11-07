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
