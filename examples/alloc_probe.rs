//! Repeats one embedder operation so an allocation profiler can count what
//! it costs: run it twice with different counts under `heaptrack` and
//! divide the difference.
//!
//! ```sh
//! cargo build --release --example alloc_probe
//! heaptrack target/release/examples/alloc_probe start 100
//! ```
//!
//! Modes: `start` (a runtime made and dropped), `floor` (runtimes kept
//! alive), `compile` (`a + b * 2`
//! compiled from new text each time), `execute` (a cached compile of
//! `a + b * 2` executed, `a` and `b` globals), `exotic` (the same with `a`
//! and `b` resolved by an exotic prototype of the global object), `call` (a
//! cached function called with two arguments), `call4` (one called with
//! four).

use std::cell::RefCell;
use std::collections::HashMap;

use quickjs_oxide::engine::api::core::{
    ExoticClass, ExoticScope, Hook, JsResult, JsString, PropertyKey, Runtime, Value,
};

struct Names;

impl ExoticClass for Names {
    type Data = RefCell<HashMap<JsString, Value>>;

    fn has(
        &self,
        _: &mut ExoticScope<'_>,
        map: &Self::Data,
        key: &PropertyKey,
    ) -> JsResult<Hook<bool>> {
        let found = key
            .to_js_string()
            .is_some_and(|name| map.borrow().contains_key(&name));
        Ok(if found {
            Hook::Handled(true)
        } else {
            Hook::Ordinary
        })
    }

    fn get(
        &self,
        _: &mut ExoticScope<'_>,
        map: &Self::Data,
        key: &PropertyKey,
    ) -> JsResult<Hook<Value>> {
        let found = key
            .to_js_string()
            .and_then(|name| map.borrow().get(&name).cloned());
        Ok(found.map_or(Hook::Ordinary, Hook::Handled))
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().expect("a mode");
    let count: usize = args.next().map_or(1, |count| count.parse().unwrap());
    match mode.as_str() {
        "start" => {
            for _ in 0..count {
                let runtime = Runtime::new();
                drop(runtime.context());
            }
        }
        "floor" => {
            // Runtimes kept alive: the peak grows by one runtime's floor each.
            let runtimes: Vec<Runtime> = (0..count).map(|_| Runtime::new()).collect();
            drop(runtimes);
        }
        "compile" => {
            let runtime = Runtime::new();
            let context = runtime.context();
            for index in 0..count {
                // New text each time, as an expression cache miss is.
                let source = format!("a + b * {}", index % 7 + 2);
                drop(context.compile(&source).unwrap());
            }
        }
        "execute" | "exotic" => {
            let runtime = Runtime::new();
            let context = runtime.context();
            if mode == "exotic" {
                let class = context.register_exotic_class(Names).unwrap();
                let map = RefCell::new(HashMap::from([
                    (JsString::from_static("a"), Value::Int(1)),
                    (JsString::from_static("b"), Value::Int(2)),
                ]));
                let prototype = context.object_prototype().unwrap();
                let names = context
                    .new_host_exotic(map, &class, Some(&prototype))
                    .unwrap();
                let global = context.global_object().unwrap();
                context.set_prototype_of(&global, Some(&names)).unwrap();
            } else {
                context.eval("var a = 1, b = 2;").unwrap();
            }
            let script = context.compile("a + b * 2").unwrap();
            for _ in 0..count {
                assert_eq!(context.execute(&script).unwrap(), Value::Int(5));
            }
        }
        "call" => {
            let runtime = Runtime::new();
            let context = runtime.context();
            let Value::Object(function) = context
                .eval("(function (a, b) { return a + b * 2; })")
                .unwrap()
            else {
                panic!()
            };
            let function = context.as_callable(&function).unwrap().unwrap();
            for _ in 0..count {
                let result = context
                    .call(&function, Value::Undefined, &[Value::Int(1), Value::Int(2)])
                    .unwrap();
                assert_eq!(result, Value::Int(5));
            }
        }
        "call4" => {
            let runtime = Runtime::new();
            let context = runtime.context();
            let Value::Object(function) = context
                .eval("(function (a, b, c, d) { return a + b * 2 + c - d; })")
                .unwrap()
            else {
                panic!()
            };
            let function = context.as_callable(&function).unwrap().unwrap();
            let arguments = [Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)];
            for _ in 0..count {
                let result = context
                    .call(&function, Value::Undefined, &arguments)
                    .unwrap();
                assert_eq!(result, Value::Int(4));
            }
        }
        other => panic!("unknown mode {other}"),
    }
}
