//! Host exotic objects: an embedder's class whose hooks are the internal
//! methods of its objects, through the public API.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use quickjs_oxide::engine::api::core::{
    CompleteOrdinaryPropertyDescriptor, Context, DescriptorField, ExoticClass, ExoticScope, Hook,
    HostClass, JsResult, JsString, NativeErrorKind, ObjectRef, OrdinaryPropertyDescriptor,
    PropertyKey, Runtime, RuntimeError, Scope, Value,
};

fn runtime() -> (Runtime, Context) {
    let runtime = Runtime::new();
    let context = runtime.context();
    (runtime, context)
}

fn text(value: &str) -> Value {
    Value::String(JsString::try_from_utf8(value).unwrap())
}

fn name(key: &PropertyKey) -> String {
    match key.to_js_string() {
        Some(text) => text.to_utf8_lossy(),
        None => "<symbol>".into(),
    }
}

fn eval_string(context: &Context, source: &str) -> String {
    match context.eval(source) {
        Ok(Value::String(text)) => text.to_utf8_lossy(),
        other => panic!("{source}: not a string: {other:?}"),
    }
}

/// The printed value `result` threw.
fn thrown<T: std::fmt::Debug>(context: &Context, result: Result<T, RuntimeError>) -> String {
    match result {
        // The first line: the message without the stack.
        Err(RuntimeError::Exception(exception)) => context
            .runtime()
            .print_value(&exception)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string(),
        other => panic!("not an exception: {other:?}"),
    }
}

/// A map of names to values with a log of the hooks called: every hook
/// answers from the map.
#[derive(Default)]
struct Bag {
    values: RefCell<BTreeMap<String, Value>>,
    log: RefCell<Vec<String>>,
}

impl Bag {
    fn with(entries: &[(&str, Value)]) -> Self {
        let bag = Bag::default();
        for (key, value) in entries {
            bag.values.borrow_mut().insert((*key).into(), value.clone());
        }
        bag
    }

    fn note(&self, entry: String) {
        self.log.borrow_mut().push(entry);
    }

    fn take_log(&self) -> Vec<String> {
        std::mem::take(&mut self.log.borrow_mut())
    }
}

struct BagClass;

impl ExoticClass for BagClass {
    type Data = Bag;

    fn get_own_property(
        &self,
        _: &mut ExoticScope<'_>,
        bag: &Bag,
        key: &PropertyKey,
    ) -> JsResult<Hook<Option<CompleteOrdinaryPropertyDescriptor>>> {
        bag.note(format!("getOwnProperty {}", name(key)));
        Ok(Hook::Handled(bag.values.borrow().get(&name(key)).map(
            |value| CompleteOrdinaryPropertyDescriptor::Data {
                value: value.clone(),
                writable: true,
                enumerable: true,
                configurable: true,
            },
        )))
    }

    fn define_own_property(
        &self,
        _: &mut ExoticScope<'_>,
        bag: &Bag,
        key: &PropertyKey,
        descriptor: &OrdinaryPropertyDescriptor,
    ) -> JsResult<Hook<bool>> {
        bag.note(format!("defineOwnProperty {}", name(key)));
        let DescriptorField::Present(value) = &descriptor.value else {
            return Ok(Hook::Handled(false));
        };
        bag.values.borrow_mut().insert(name(key), value.clone());
        Ok(Hook::Handled(true))
    }

    fn has(&self, _: &mut ExoticScope<'_>, bag: &Bag, key: &PropertyKey) -> JsResult<Hook<bool>> {
        bag.note(format!("has {}", name(key)));
        if bag.values.borrow().contains_key(&name(key)) {
            return Ok(Hook::Handled(true));
        }
        // Not here: the prototype decides.
        Ok(Hook::Ordinary)
    }

    fn get(&self, _: &mut ExoticScope<'_>, bag: &Bag, key: &PropertyKey) -> JsResult<Hook<Value>> {
        bag.note(format!("get {}", name(key)));
        match bag.values.borrow().get(&name(key)) {
            Some(value) => Ok(Hook::Handled(value.clone())),
            None => Ok(Hook::Ordinary),
        }
    }

    fn set(
        &self,
        scope: &mut ExoticScope<'_>,
        bag: &Bag,
        key: &PropertyKey,
        value: Value,
    ) -> JsResult<Hook<bool>> {
        let own = scope.receiver_is_this();
        bag.note(format!("set {} own={own}", name(key)));
        if name(key) == "readonly" {
            return Ok(Hook::Handled(false));
        }
        if !own {
            // An assignment through the prototype chain: the receiver gets
            // the property, as `OrdinarySet` would give it.
            return Ok(Hook::Ordinary);
        }
        bag.values.borrow_mut().insert(name(key), value);
        Ok(Hook::Handled(true))
    }

    fn delete(
        &self,
        _: &mut ExoticScope<'_>,
        bag: &Bag,
        key: &PropertyKey,
    ) -> JsResult<Hook<bool>> {
        bag.note(format!("delete {}", name(key)));
        bag.values.borrow_mut().remove(&name(key));
        Ok(Hook::Handled(true))
    }

    fn own_keys(&self, _: &mut ExoticScope<'_>, bag: &Bag) -> JsResult<Hook<Vec<PropertyKey>>> {
        bag.note("ownKeys".into());
        Ok(Hook::Handled(
            bag.values
                .borrow()
                .keys()
                .map(|key| PropertyKey::from_js_string(JsString::try_from_utf8(key).unwrap()))
                .collect(),
        ))
    }
}

/// A bag on the global object as `name`.
fn global_bag(context: &Context, name: &str, bag: Bag) -> (HostClass<BagClass>, ObjectRef) {
    let class = context.register_exotic_class(BagClass).unwrap();
    let prototype = context.object_prototype().unwrap();
    let object = context
        .new_host_exotic(bag, &class, Some(&prototype))
        .unwrap();
    let global = context.global_object().unwrap();
    let key = context.runtime().intern_property_key(name).unwrap();
    context
        .set_property(&global, &key, Value::Object(object.clone()))
        .unwrap();
    (class, object)
}

fn bag_of(context: &Context, object: &ObjectRef) -> Rc<Bag> {
    context.host_data::<Bag>(object).unwrap().unwrap()
}

#[test]
fn every_hook_answers_its_internal_method() {
    let (_, context) = runtime();
    let (_, object) = global_bag(&context, "bag", Bag::with(&[("a", Value::Int(1))]));
    let bag = bag_of(&context, &object);

    assert_eq!(context.eval("bag.a").unwrap(), Value::Int(1));
    assert_eq!(bag.take_log(), ["get a"]);

    assert_eq!(context.eval("bag.b = 2; bag.b").unwrap(), Value::Int(2));
    assert_eq!(bag.take_log(), ["set b own=true", "get b"]);

    assert_eq!(context.eval("'b' in bag").unwrap(), Value::Bool(true));
    assert_eq!(bag.take_log(), ["has b"]);

    assert_eq!(
        eval_string(
            &context,
            "JSON.stringify(Object.getOwnPropertyDescriptor(bag, 'a'))"
        ),
        r#"{"value":1,"writable":true,"enumerable":true,"configurable":true}"#
    );
    assert_eq!(bag.take_log(), ["getOwnProperty a"]);

    context
        .eval("Object.defineProperty(bag, 'c', { value: 3 })")
        .unwrap();
    assert_eq!(bag.take_log(), ["defineOwnProperty c"]);
    assert_eq!(bag.values.borrow().get("c"), Some(&Value::Int(3)));

    assert_eq!(context.eval("delete bag.c").unwrap(), Value::Bool(true));
    assert_eq!(bag.take_log(), ["delete c"]);
    assert!(!bag.values.borrow().contains_key("c"));

    assert_eq!(eval_string(&context, "Reflect.ownKeys(bag).join()"), "a,b");
    assert_eq!(bag.take_log(), ["ownKeys"]);
    assert_eq!(eval_string(&context, "Object.keys(bag).join()"), "a,b");
    assert_eq!(
        eval_string(
            &context,
            "var seen = []; for (var k in bag) seen.push(k); seen.join()"
        ),
        "a,b"
    );
    assert_eq!(
        eval_string(&context, "JSON.stringify(bag)"),
        r#"{"a":1,"b":2}"#
    );
    assert_eq!(
        eval_string(&context, "JSON.stringify(Object.assign({}, bag))"),
        r#"{"a":1,"b":2}"#
    );
    assert_eq!(
        eval_string(&context, "JSON.stringify({ ...bag })"),
        r#"{"a":1,"b":2}"#
    );

    // A failed set is a TypeError in strict code and nothing in sloppy.
    assert_eq!(context.eval("bag.readonly = 1").unwrap(), Value::Int(1));
    assert_eq!(
        thrown(&context, context.eval("'use strict'; bag.readonly = 1")),
        "TypeError: 'readonly' is read-only"
    );
    assert_eq!(context.eval("typeof bag").unwrap(), text("object"));
}

#[test]
fn a_hook_reaches_its_object_and_the_engine_through_its_scope() {
    struct Counter;
    impl ExoticClass for Counter {
        type Data = Cell<i32>;

        fn get(
            &self,
            scope: &mut ExoticScope<'_>,
            count: &Cell<i32>,
            key: &PropertyKey,
        ) -> JsResult<Hook<Value>> {
            match name(key).as_str() {
                "self" => Ok(Hook::Handled(Value::Object(scope.this()))),
                "receiver" => Ok(Hook::Handled(scope.receiver())),
                "data" => {
                    let this = scope.this();
                    let data = scope.host_data::<Cell<i32>>(&this)?.unwrap();
                    Ok(Hook::Handled(Value::Bool(std::ptr::eq(&*data, count))))
                }
                "made" => {
                    // A hook makes values and runs JavaScript.
                    count.set(count.get() + 1);
                    let object = scope.new_object()?;
                    let key = PropertyKey::from_js_string(JsString::from_static("n"));
                    scope.set_property(&object, &key, Value::Int(count.get()))?;
                    let doubled = scope.eval("21 * 2")?;
                    let key = PropertyKey::from_js_string(JsString::from_static("d"));
                    scope.set_property(&object, &key, doubled)?;
                    Ok(Hook::Handled(Value::Object(object)))
                }
                "fail" => Err(scope.throw_error(NativeErrorKind::Range, "no such thing")),
                _ => Ok(Hook::Ordinary),
            }
        }
    }
    let (_, context) = runtime();
    let class = context.register_exotic_class(Counter).unwrap();
    let object = context.new_host_exotic(Cell::new(0), &class, None).unwrap();
    let global = context.global_object().unwrap();
    let key = context.runtime().intern_property_key("counter").unwrap();
    context
        .set_property(&global, &key, Value::Object(object.clone()))
        .unwrap();
    assert_eq!(
        context.eval("counter.self === counter").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        context.eval("counter.receiver === counter").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        context
            .eval("var child = Object.create(counter); child.receiver === child")
            .unwrap(),
        Value::Bool(true)
    );
    assert_eq!(context.eval("counter.data").unwrap(), Value::Bool(true));
    assert_eq!(
        eval_string(&context, "JSON.stringify([counter.made, counter.made])"),
        r#"[{"n":1,"d":42},{"n":2,"d":42}]"#
    );
    assert_eq!(
        eval_string(
            &context,
            "try { counter.fail } catch (e) { e instanceof RangeError && e.message }"
        ),
        "no such thing"
    );
    // A declined hook with a null prototype: the ordinary method finds
    // nothing.
    assert_eq!(context.eval("counter.other").unwrap(), Value::Undefined);
    assert_eq!(
        context
            .host_data::<Cell<i32>>(&object)
            .unwrap()
            .unwrap()
            .get(),
        2
    );
}

#[test]
fn a_class_without_hooks_is_an_ordinary_object() {
    struct Plain;
    impl ExoticClass for Plain {
        type Data = ();
    }
    let (_, context) = runtime();
    let class = context.register_exotic_class(Plain).unwrap();
    let prototype = context.eval("({ inherited: 'yes' })").unwrap();
    let Value::Object(prototype) = prototype else {
        panic!()
    };
    let object = context
        .new_host_exotic((), &class, Some(&prototype))
        .unwrap();
    let global = context.global_object().unwrap();
    let key = context.runtime().intern_property_key("plain").unwrap();
    context
        .set_property(&global, &key, Value::Object(object))
        .unwrap();
    assert_eq!(
        eval_string(
            &context,
            "plain.x = 1; plain[0] = 'zero'; Object.defineProperty(plain, 'y', { value: 2 });
             [plain.x, plain[0], plain.y, plain.inherited, 'x' in plain, 'inherited' in plain,
              Object.keys(plain).join('|'), delete plain.x, 'x' in plain, delete plain.y].join()"
        ),
        "1,zero,2,yes,true,true,0|x,true,false,false"
    );
}

#[test]
fn a_set_through_the_prototype_chain_sees_the_ordinary_receiver() {
    let (_, context) = runtime();
    let (_, object) = global_bag(&context, "bag", Bag::default());
    let bag = bag_of(&context, &object);
    assert_eq!(
        eval_string(
            &context,
            "var child = Object.create(bag); child.x = 5;
             [child.x, Object.getOwnPropertyNames(child).join(), 'x' in bag].join()"
        ),
        "5,x,false"
    );
    // The hook saw the child as the receiver, declined, and `OrdinarySet`
    // asked the bag for its own `x` before defining it on the child.
    assert_eq!(
        bag.take_log(),
        [
            "set x own=false",
            "getOwnProperty x",
            "has x",
            "getOwnProperty x"
        ]
    );
    // The child's own property now shadows the bag.
    context.eval("child.x = 6").unwrap();
    assert_eq!(bag.take_log(), Vec::<String>::new());
    // Reflect.set with an explicit receiver.
    assert_eq!(
        eval_string(
            &context,
            "var other = {}; Reflect.set(bag, 'y', 7, other);
             [other.y, 'y' in bag].join()"
        ),
        "7,false"
    );
    assert_eq!(
        bag.take_log(),
        [
            "set y own=false",
            "getOwnProperty y",
            "has y",
            "getOwnProperty y"
        ]
    );
}

#[test]
fn the_global_prototype_resolves_bare_names() {
    let (_, context) = runtime();
    let class = context.register_exotic_class(BagClass).unwrap();
    let object_prototype = context.object_prototype().unwrap();
    let names = context
        .new_host_exotic(
            Bag::with(&[("known", Value::Int(10))]),
            &class,
            Some(&object_prototype),
        )
        .unwrap();
    let global = context.global_object().unwrap();
    assert!(context.set_prototype_of(&global, Some(&names)).unwrap());
    let bag = bag_of(&context, &names);

    // Read.
    assert_eq!(context.eval("known + 1").unwrap(), Value::Int(11));
    assert_eq!(bag.take_log(), ["has known", "get known"]);
    // Object.prototype stays reachable behind the exotic prototype.
    assert_eq!(
        context.eval("typeof hasOwnProperty").unwrap(),
        text("function")
    );
    bag.take_log();
    // An unresolvable name: the hook declines, the chain ends.
    assert_eq!(
        thrown(&context, context.eval("missing")),
        "ReferenceError: 'missing' is not defined"
    );
    assert_eq!(bag.take_log(), ["has missing", "getOwnProperty missing"]);

    // typeof.
    assert_eq!(context.eval("typeof known").unwrap(), text("number"));
    assert_eq!(context.eval("typeof missing").unwrap(), text("undefined"));
    assert_eq!(
        bag.take_log(),
        [
            "has known",
            "get known",
            "has missing",
            "getOwnProperty missing"
        ]
    );

    // Write, sloppy: the receiver is the global object.
    context.eval("known = 20").unwrap();
    assert_eq!(
        bag.take_log(),
        ["set known own=false", "getOwnProperty known"]
    );
    // The hook declined for a foreign receiver, so the global object got
    // its own `known`, which now shadows the prototype's.
    assert_eq!(context.eval("known").unwrap(), Value::Int(20));
    assert_eq!(bag.values.borrow().get("known"), Some(&Value::Int(10)));
    assert_eq!(bag.take_log(), Vec::<String>::new());

    // Write, strict, to a name the prototype resolves: `has`, then `set`.
    bag.values
        .borrow_mut()
        .insert("other".into(), Value::Int(1));
    context.eval("'use strict'; other = 2").unwrap();
    assert_eq!(
        bag.take_log(),
        ["has other", "set other own=false", "getOwnProperty other"]
    );
    // Write, strict, to an unresolvable name: `has` only, then the error.
    assert_eq!(
        thrown(&context, context.eval("'use strict'; nowhere = 1")),
        "ReferenceError: 'nowhere' is not defined"
    );
    assert_eq!(bag.take_log(), ["has nowhere", "getOwnProperty nowhere"]);
    // Sloppy creates it on the global object.
    context.eval("nowhere = 1").unwrap();
    assert_eq!(
        context
            .eval("globalThis.hasOwnProperty('nowhere')")
            .unwrap(),
        Value::Bool(true)
    );
    bag.take_log();

    // `in` on the global object asks the prototype.
    bag.values.borrow_mut().insert("inside".into(), Value::Null);
    assert_eq!(
        context.eval("'inside' in globalThis").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        context.eval("'outside' in globalThis").unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        bag.take_log(),
        ["has inside", "has outside", "getOwnProperty outside"]
    );

    // `delete name` deletes from the global object, not its prototype;
    // the prototype's own property goes through its `delete` hook.
    assert_eq!(context.eval("delete inside").unwrap(), Value::Bool(true));
    assert_eq!(bag.take_log(), Vec::<String>::new());
    assert_eq!(
        context
            .eval("delete Object.getPrototypeOf(globalThis).inside")
            .unwrap(),
        Value::Bool(true)
    );
    assert_eq!(bag.take_log(), ["delete inside"]);
    assert_eq!(context.eval("typeof inside").unwrap(), text("undefined"));

    // Inside a function and a module-free closure the names resolve the
    // same way.
    bag.values.borrow_mut().insert("late".into(), Value::Int(3));
    assert_eq!(
        context
            .eval("(function () { return late * 2; })()")
            .unwrap(),
        Value::Int(6)
    );
}

/// A class whose `get` answers a fresh number each time.
struct Ticker;

impl ExoticClass for Ticker {
    type Data = Cell<i32>;

    fn has(
        &self,
        _: &mut ExoticScope<'_>,
        _: &Cell<i32>,
        key: &PropertyKey,
    ) -> JsResult<Hook<bool>> {
        Ok(Hook::Handled(name(key) == "tick"))
    }

    fn get(
        &self,
        _: &mut ExoticScope<'_>,
        count: &Cell<i32>,
        key: &PropertyKey,
    ) -> JsResult<Hook<Value>> {
        if name(key) != "tick" {
            return Ok(Hook::Ordinary);
        }
        count.set(count.get() + 1);
        Ok(Hook::Handled(Value::Int(count.get())))
    }
}

#[test]
fn inline_caches_never_hold_a_hooks_answer() {
    let (_, context) = runtime();
    let class = context.register_exotic_class(Ticker).unwrap();
    let ticker = context.new_host_exotic(Cell::new(0), &class, None).unwrap();
    let global = context.global_object().unwrap();
    let key = context.runtime().intern_property_key("ticker").unwrap();
    context
        .set_property(&global, &key, Value::Object(ticker.clone()))
        .unwrap();
    // A named read at one site, on the object and through an ordinary
    // object whose prototype it is.
    assert_eq!(
        eval_string(
            &context,
            "var out = [];
             for (var i = 0; i < 5; i++) out.push(ticker.tick);
             var child = Object.create(ticker);
             for (var i = 0; i < 5; i++) out.push(child.tick);
             function read(o) { return o.tick; }
             for (var i = 0; i < 5; i++) out.push(read(i % 2 ? child : ticker));
             out.join()"
        ),
        "1,2,3,4,5,6,7,8,9,10,11,12,13,14,15"
    );
    // A bare name through the global object's prototype.
    let object_prototype = context.object_prototype().unwrap();
    let names = context
        .new_host_exotic(Cell::new(100), &class, Some(&object_prototype))
        .unwrap();
    assert!(context.set_prototype_of(&global, Some(&names)).unwrap());
    assert_eq!(
        eval_string(
            &context,
            "var out = []; for (var i = 0; i < 5; i++) out.push(tick); out.join()"
        ),
        "101,102,103,104,105"
    );
    assert_eq!(
        eval_string(
            &context,
            "(function () { var out = []; for (var i = 0; i < 5; i++) out.push(typeof tick, tick); return out.join(); })()"
        ),
        // `typeof tick` reads it too.
        "number,107,number,109,number,111,number,113,number,115"
    );
}

#[test]
fn the_host_datum_drops_when_the_object_is_collected() {
    struct Tracked(Rc<Cell<u32>>);
    impl Drop for Tracked {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }
    struct Class;
    impl ExoticClass for Class {
        type Data = Tracked;
    }
    let (runtime, context) = runtime();
    let drops = Rc::new(Cell::new(0));
    let class = context.register_exotic_class(Class).unwrap();
    let object = context
        .new_host_exotic(Tracked(Rc::clone(&drops)), &class, None)
        .unwrap();
    let global = context.global_object().unwrap();
    let key = runtime.intern_property_key("tracked").unwrap();
    context
        .set_property(&global, &key, Value::Object(object.clone()))
        .unwrap();
    for _ in 0..3 {
        context
            .new_host_exotic(Tracked(Rc::clone(&drops)), &class, None)
            .unwrap();
    }
    runtime.run_gc().unwrap();
    assert_eq!(drops.get(), 3);
    drop(object);
    runtime.run_gc().unwrap();
    assert_eq!(drops.get(), 3);
    context.eval("tracked = null").unwrap();
    runtime.run_gc().unwrap();
    assert_eq!(drops.get(), 4);
    // The class outlives its objects: a new one works.
    context
        .new_host_exotic(Tracked(Rc::clone(&drops)), &class, None)
        .unwrap();
    drop(context);
    drop(runtime);
    assert_eq!(drops.get(), 5);
}

#[test]
fn a_class_belongs_to_its_runtime() {
    struct Class;
    impl ExoticClass for Class {
        type Data = ();
    }
    let (_, context) = runtime();
    let (_, other) = runtime();
    let class = context.register_exotic_class(Class).unwrap();
    assert_eq!(
        other.new_host_exotic((), &class, None).map(drop),
        Err(RuntimeError::Invariant("a class of another runtime"))
    );
}

#[test]
fn a_panic_in_a_hook_poisons_the_runtime() {
    struct Class;
    impl ExoticClass for Class {
        type Data = ();
        fn get(&self, _: &mut ExoticScope<'_>, _: &(), _: &PropertyKey) -> JsResult<Hook<Value>> {
            panic!("hook bug")
        }
    }
    let (runtime, context) = runtime();
    let class = context.register_exotic_class(Class).unwrap();
    let object = context.new_host_exotic((), &class, None).unwrap();
    let global = context.global_object().unwrap();
    let key = runtime.intern_property_key("boom").unwrap();
    context
        .set_property(&global, &key, Value::Object(object))
        .unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| context.eval("boom.x")));
    assert!(outcome.is_err());
    assert_eq!(context.eval("1"), Err(RuntimeError::Poisoned));
}

#[test]
fn hooks_recurse_against_the_native_stack_budget() {
    struct Deep;
    impl ExoticClass for Deep {
        type Data = ();
        fn get(
            &self,
            scope: &mut ExoticScope<'_>,
            _: &(),
            _: &PropertyKey,
        ) -> JsResult<Hook<Value>> {
            // Each read reads itself again.
            scope.eval("deep.again").map(Hook::Handled)
        }
    }
    let (runtime, context) = runtime();
    let class = context.register_exotic_class(Deep).unwrap();
    let object = context.new_host_exotic((), &class, None).unwrap();
    let global = context.global_object().unwrap();
    let key = runtime.intern_property_key("deep").unwrap();
    context
        .set_property(&global, &key, Value::Object(object))
        .unwrap();
    assert_eq!(
        thrown(&context, context.eval("deep.x")),
        "InternalError: stack overflow"
    );
    assert_eq!(context.eval("1").unwrap(), Value::Int(1));
}

#[test]
fn a_host_function_and_a_hook_share_one_runtime() {
    let (_, context) = runtime();
    let (_, object) = global_bag(&context, "bag", Bag::with(&[("n", Value::Int(4))]));
    let global = context.global_object().unwrap();
    // A host function reads the bag through its scope; the hook runs
    // nested inside the host function.
    context
        .define_function(&global, "readN", 0, |scope: &mut Scope<'_>| {
            let bag = scope.arg(0);
            let Value::Object(bag) = bag else {
                return Err(scope.throw_error(NativeErrorKind::Type, "not a bag"));
            };
            let key = PropertyKey::from_js_string(JsString::from_static("n"));
            scope.get_property(&bag, &key)
        })
        .unwrap();
    assert_eq!(context.eval("readN(bag) * 2").unwrap(), Value::Int(8));
    let bag = bag_of(&context, &object);
    assert_eq!(bag.take_log(), ["get n"]);
}

#[test]
fn proxies_are_unchanged() {
    let (_, context) = runtime();
    assert_eq!(
        eval_string(
            &context,
            "var log = [];
             var p = new Proxy({ a: 1 }, {
               get(t, k, r) { log.push('get ' + String(k)); return Reflect.get(t, k, r); },
               set(t, k, v, r) { log.push('set ' + k); return Reflect.set(t, k, v, r); },
               has(t, k) { log.push('has ' + k); return k in t; },
               deleteProperty(t, k) { log.push('delete ' + k); return delete t[k]; },
               ownKeys(t) { log.push('ownKeys'); return Reflect.ownKeys(t); },
               getOwnPropertyDescriptor(t, k) { log.push('gopd ' + k); return Reflect.getOwnPropertyDescriptor(t, k); },
               defineProperty(t, k, d) { log.push('define ' + k); return Reflect.defineProperty(t, k, d); },
             });
             for (var i = 0; i < 3; i++) p.a;
             p.b = 2; 'a' in p; delete p.b; Object.keys(p);
             var c = Object.create(p); c.z = 1;
             log.join('|')"
        ),
        "get a|get a|get a|set b|gopd b|define b|has a|delete b|ownKeys|gopd a|set z"
    );
    // A proxy as the global object's prototype.
    assert_eq!(
        eval_string(
            &context,
            "var seen = [];
             Object.setPrototypeOf(globalThis, new Proxy(Object.getPrototypeOf(globalThis), {
               has(t, k) { seen.push('has ' + String(k)); return k === 'viaProxy' || k in t; },
               get(t, k, r) { if (k === 'viaProxy') return 'p'; return Reflect.get(t, k, r); },
             }));
             (0, eval)('viaProxy') + typeof nothingHere + seen.filter(s => s.indexOf('Symbol') < 0).join()"
        ),
        "pundefinedhas viaProxy,has nothingHere"
    );
}
