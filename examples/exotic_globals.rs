//! Host exotic objects: bare global names resolved by a Rust map through an
//! exotic prototype of the global object, and what 1,000 reads and writes
//! of such a name cost.
//!
//! ```sh
//! cargo run --quiet --release --example exotic_globals
//! ```

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;

use quickjs_oxide::engine::api::core::{
    ExoticClass, ExoticScope, Hook, JsResult, JsString, PropertyKey, Runtime, Value,
};

/// The names a script sees beside its own globals, as an embedder keeps
/// them: a Rust map, keyed by the engine's strings so a lookup copies no
/// text.
#[derive(Default)]
struct Names {
    values: RefCell<HashMap<JsString, Value>>,
}

/// A key as a map key; `None` for a symbol.
fn name(key: &PropertyKey) -> Option<JsString> {
    key.to_js_string()
}

struct NamesClass;

impl ExoticClass for NamesClass {
    type Data = Names;

    fn has(
        &self,
        _: &mut ExoticScope<'_>,
        names: &Names,
        key: &PropertyKey,
    ) -> JsResult<Hook<bool>> {
        match name(key) {
            Some(name) if names.values.borrow().contains_key(&name) => Ok(Hook::Handled(true)),
            // `Object.prototype` behind the map answers the rest.
            _ => Ok(Hook::Ordinary),
        }
    }

    fn get(
        &self,
        _: &mut ExoticScope<'_>,
        names: &Names,
        key: &PropertyKey,
    ) -> JsResult<Hook<Value>> {
        let found = name(key).and_then(|name| names.values.borrow().get(&name).cloned());
        Ok(found.map_or(Hook::Ordinary, Hook::Handled))
    }

    fn set(
        &self,
        _: &mut ExoticScope<'_>,
        names: &Names,
        key: &PropertyKey,
        value: Value,
    ) -> JsResult<Hook<bool>> {
        // A name the map holds is written there; any other becomes a
        // global of the script's own.
        let Some(name) = name(key) else {
            return Ok(Hook::Ordinary);
        };
        match names.values.borrow_mut().get_mut(&name) {
            Some(slot) => {
                *slot = value;
                Ok(Hook::Handled(true))
            }
            None => Ok(Hook::Ordinary),
        }
    }
}

fn main() {
    let runtime = Runtime::new();
    let context = runtime.context();
    let class = context.register_exotic_class(NamesClass).unwrap();
    let object_prototype = context.object_prototype().unwrap();
    let names = Names::default();
    names
        .values
        .borrow_mut()
        .insert(JsString::from_static("counter"), Value::Int(0));
    let names = context
        .new_host_exotic(names, &class, Some(&object_prototype))
        .unwrap();
    let global = context.global_object().unwrap();
    context.set_prototype_of(&global, Some(&names)).unwrap();

    let total = context
        .eval("for (let i = 0; i < 10; i++) counter += 1; counter")
        .unwrap();
    assert_eq!(total, Value::Int(10));

    // Each run is 1,000 iterations; the loop variable is a local.
    let cases = [
        (
            "1000 adds (counter += 1)",
            "(function () { for (let i = 0; i < 1000; i++) counter += 1; })()",
        ),
        (
            "1000 reads",
            "(function () { let s = 0; for (let i = 0; i < 1000; i++) s = counter; return s; })()",
        ),
        (
            "1000 writes",
            "(function () { for (let i = 0; i < 1000; i++) counter = i; })()",
        ),
        (
            "1000 adds to an ordinary global",
            "var plain = 0; (function () { for (let i = 0; i < 1000; i++) plain += 1; })()",
        ),
    ];
    const RUNS: u32 = 2_000;
    for (label, source) in cases {
        let script = context.compile(source).unwrap();
        for _ in 0..50 {
            context.execute(&script).unwrap();
        }
        let start = Instant::now();
        for _ in 0..RUNS {
            context.execute(&script).unwrap();
        }
        let per_run = start.elapsed() / RUNS;
        println!("{label}: {:.1} µs", per_run.as_secs_f64() * 1e6);
    }
    let held = names_value(&context, &names);
    println!("counter = {held:?}");
}

fn names_value(
    context: &quickjs_oxide::engine::api::core::Context,
    names: &quickjs_oxide::engine::api::core::ObjectRef,
) -> Value {
    let names = context.host_data::<Names>(names).unwrap().unwrap();
    names
        .values
        .borrow()
        .get(&JsString::from_static("counter"))
        .cloned()
        .unwrap()
}
