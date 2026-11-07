//! Runs a script compiled at the top level, inside a host function, or
//! loaded from an image inside a host function, to compare their
//! instruction counts (`perf stat -e instructions:u`).
//!
//! ```sh
//! cargo build --release --example host_compile
//! perf stat -e instructions:u target/release/examples/host_compile top program.js
//! perf stat -e instructions:u target/release/examples/host_compile host program.js
//! ```
//!
//! Modes: `top` (compiled and run at the top level), `host` (compiled and
//! run by a host function), `transient` (the same with transient code),
//! `image` (an image loaded and run by Rust code a host function lends its
//! engine to; the image is compiled at the top level first).

use quickjs_oxide::engine::api::core::{CompileOptions, QjsHelpers, Runtime, Scope, Value};

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().expect("a mode: top, host, transient or image");
    let path = args.next().expect("a script");
    let source = std::fs::read_to_string(&path).unwrap();
    let runtime = Runtime::new();
    let context = runtime.context();
    context.install_qjs_helpers(&QjsHelpers::default()).unwrap();
    let global = context.global_object().unwrap();
    let run = |source: String, transient: bool| {
        move |scope: &mut Scope<'_>| {
            let options = CompileOptions {
                filename: "program.js".into(),
                transient,
            };
            let script = scope.compile_with_options(&source, &options)?;
            scope.execute(&script)
        }
    };
    match mode.as_str() {
        "top" => {
            let script = context
                .compile_with_filename(&source, "program.js")
                .unwrap();
            context.execute(&script).unwrap();
        }
        "host" | "transient" => {
            context
                .define_function(&global, "run", 0, run(source, mode == "transient"))
                .unwrap();
            context.eval("run()").unwrap();
        }
        "host-again" | "image-again" => {
            // One host function compiles (or loads) the script without
            // running it; after the entry returns, another runs it inside a
            // host function: the steady state of a cached nested script.
            let slot = std::rc::Rc::new(std::cell::RefCell::new(None));
            let store = std::rc::Rc::clone(&slot);
            let image = (mode == "image-again")
                .then(|| context.compile_image(&source, "program.js").unwrap());
            context
                .define_function(&global, "prepare", 0, move |scope: &mut Scope<'_>| {
                    let script = match &image {
                        Some(image) => scope.load_image(image)?,
                        None => scope.compile_with_filename(&source, "program.js")?,
                    };
                    *store.borrow_mut() = Some(script);
                    Ok(Value::Undefined)
                })
                .unwrap();
            context
                .define_function(&global, "run", 0, move |scope: &mut Scope<'_>| {
                    let script = slot.borrow().clone().expect("prepared");
                    scope.execute(&script)
                })
                .unwrap();
            context.eval("prepare()").unwrap();
            context.eval("run()").unwrap();
        }
        "image" => {
            let image = context.compile_image(&source, "program.js").unwrap();
            let nested = context.clone();
            context
                .define_function(&global, "run", 0, move |scope: &mut Scope<'_>| {
                    scope.lend(|| {
                        let script = nested.load_image(&image)?;
                        nested.execute(&script)
                    })
                })
                .unwrap();
            context.eval("run()").unwrap();
        }
        other => panic!("unknown mode {other}"),
    }
    let _ = Value::Undefined;
}
