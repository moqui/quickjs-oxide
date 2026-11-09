//! Code images: a script compiled in one runtime, loaded into another by
//! value, runs as the compiled script does.

use quickjs_oxide::engine::api::core::{
    CodeImage, Context, Runtime, RuntimeError, Scope, Value, front_end_parses,
};

fn runtime() -> (Runtime, Context) {
    let runtime = Runtime::new();
    let context = runtime.context();
    (runtime, context)
}

fn printed(context: &Context, value: Result<Value, RuntimeError>) -> String {
    match value {
        Ok(Value::String(text)) => text.to_utf8_lossy(),
        Ok(other) => context.runtime().print_value(&other).unwrap(),
        Err(RuntimeError::Exception(error)) => {
            format!("threw {}", context.runtime().print_value(&error).unwrap())
        }
        Err(other) => format!("{other:?}"),
    }
}

/// The source's completion value run from source and from its image in a
/// fresh runtime, which must agree.
fn round_trip(source: &str) -> String {
    let (_, compiled) = runtime();
    let expected = printed(&compiled, compiled.eval(source));
    let image = compiled.compile_image(source, "<image>").unwrap();
    let (_, loaded) = runtime();
    let script = loaded.load_image(&image).unwrap();
    let actual = printed(&loaded, loaded.execute(&script));
    assert_eq!(actual, expected, "{source}");
    actual
}

#[test]
fn every_constant_kind_round_trips() {
    assert_eq!(
        round_trip(
            "[1, -0, 0.5, 1e300, NaN, Infinity, 2147483648, -7, true, null, undefined,
              'text', 'ünïcode ☃', '', '123', 12n, -(2n ** 130n),
              typeof 'x', void 0].map(String).join('|')"
        ),
        "1|0|0.5|1e+300|NaN|Infinity|2147483648|-7|true|null|undefined|text|ünïcode ☃||123|12|-1361129467683753853853498429727072845824|string|undefined"
    );
    // A literal spelling an index stays its own string, not an atom.
    round_trip("var a = ['10', '10']; [a[0] === a[1], typeof a[0]].join()");
    round_trip("Object.is(-0, [-0][0]) + ':' + (1 / -0)");
}

#[test]
fn closures_children_and_names_round_trip() {
    assert_eq!(
        round_trip(
            "function outer(a) { let n = a; return { inc: () => ++n, get value() { return n; }, named: function inner() {} }; }
             var o = outer(5); o.inc(); o.inc();
             var anon = function () {}; var arrow = () => 1;
             [o.value, o.named.name, anon.name, arrow.name, outer.length, outer.toString().slice(0, 17)].join()"
        ),
        "7,inner,anon,arrow,1,function outer(a)"
    );
    round_trip(
        "var {a, b: [c, ...d], ...rest} = {a: 1, b: [2, 3, 4], e: 5, f: 6};
         [a, c, d, Object.keys(rest), [...'hey'], Math.max(...[1, 9, 3])].join(';')",
    );
    round_trip(
        "label: for (var i = 0; i < 3; i++) { for (var j in {x: 1, y: 2}) { if (j === 'y') continue label; } }
         var s = ''; switch (i) { case 3: s = 'three'; break; default: s = 'other'; } s + i",
    );
}

#[test]
fn regexps_bigints_and_templates_round_trip() {
    assert_eq!(
        round_trip(
            "var r = /(\\d+)-(?<word>[a-z]+)/giu; var m = r.exec('12-ab 34-cd');
             [m[1], m.groups.word, r.flags, r.lastIndex, 'aXbX'.replace(/x/gi, '_'), /a/y.sticky].join()"
        ),
        "12,ab,giu,5,a_b_,true"
    );
    round_trip("[10n ** 40n, 0n, -1n, 0x1fn, BigInt.asUintN(8, 257n)].join()");
    assert_eq!(
        round_trip(
            "function tag(strings, ...values) { return strings.raw.join('|') + '/' + strings.join('|') + '/' + values.join(); }
             function site() { return tag`a${1}\\n${2}c`; }
             var first = (function () { function t(s) { return s; } var x = []; for (var i = 0; i < 2; i++) x.push(t`same`); return x[0] === x[1] && Object.isFrozen(x[0]); })();
             [site(), first, `plain ${1 + 1} text`, tag`\\unicode`].join(' ')"
        ),
        "a|\\n|c/a|\n|c/1,2 true plain 2 text \\unicode//"
    );
}

#[test]
fn classes_generators_and_async_round_trip() {
    assert_eq!(
        round_trip(
            "class A { #secret = 1; static count = 0; field = 'f'; constructor() { A.count++; }
                 get secret() { return this.#secret; } #hidden() { return 2; } both() { return this.#hidden() + this.#secret; }
                 static { this.ready = true; } }
             class B extends A { constructor() { super(); this.extra = super.both(); } toString() { return 'B'; } }
             var b = new B(); new A();
             [b.secret, b.field, b.both(), A.count, A.ready, b.extra, String(b), #secret in b ? 'no' : 'outside'].join()"
                .replace("#secret in b ? 'no' : 'outside'", "b instanceof A")
                .as_str()
        ),
        "1,f,3,2,true,3,B,true"
    );
    assert_eq!(
        round_trip(
            "function* gen(n) { for (let i = 0; i < n; i++) yield i * 2; return 'done'; }
             var g = gen(3); var out = [...gen(4)];
             var log = [];
             async function work() { log.push('start'); await null; log.push('after'); return 'resolved'; }
             async function* agen() { yield 1; yield 2; }
             work().then(v => log.push(v));
             [out.join(), g.next().value, g.next().value, typeof agen().next, log.join()].join(';')"
        ),
        "0,2,4,6;0;2;function;start"
    );
}

#[test]
fn an_image_loads_into_a_runtime_with_other_atoms() {
    let (_, compiled) = runtime();
    let source = "var record = { alpha: 1, beta: 2, gamma: 3 };
                  function sum(o) { return o.alpha + o.beta + o.gamma; }
                  [sum(record), Object.keys(record).join(), record.delta === undefined].join(';')";
    let image = compiled.compile_image(source, "atoms.js").unwrap();
    // A runtime that interned other names first numbers its atoms
    // differently.
    let (_, loaded) = runtime();
    loaded
        .eval("var zz1 = 1, zz2 = 2, gamma = 'g', qq = { delta: 4, beta: 5, omega: 6 };")
        .unwrap();
    let script = loaded.load_image(&image).unwrap();
    assert_eq!(
        printed(&loaded, loaded.execute(&script)),
        "6;alpha,beta,gamma;true"
    );
    // The same image loads again into the same runtime, and once more
    // after a collection.
    let again = loaded.load_image(&image).unwrap();
    loaded.runtime().run_gc().unwrap();
    assert_eq!(
        printed(&loaded, loaded.execute(&again)),
        "6;alpha,beta,gamma;true"
    );
}

#[test]
fn errors_from_a_loaded_script_keep_their_positions() {
    let source = "var x = 1;\nfunction fails(o) {\n  return o.missing.deeper;\n}\nfunction call() { return fails({}); }\ntry { call(); } catch (e) { e.stack }";
    let (_, compiled) = runtime();
    let script = compiled
        .compile_with_filename(source, "positions.js")
        .unwrap();
    let expected = printed(&compiled, compiled.execute(&script));
    assert!(expected.contains("positions.js:3:"), "{expected}");
    let image = compiled.encode_image(&script).unwrap();
    let (_, loaded) = runtime();
    let script = loaded.load_image(&image).unwrap();
    assert_eq!(printed(&loaded, loaded.execute(&script)), expected);
    // A syntax error stays the compiler's.
    assert!(matches!(
        compiled.compile_image("var = 1", "bad.js"),
        Err(RuntimeError::Exception(_))
    ));
}

#[test]
fn an_image_another_thread_made_loads_without_parsing() {
    let source = "var total = 0; for (var i = 0; i < 100; i++) total += [i].map(x => x * 2)[0]; 'total ' + total";
    let image: CodeImage = std::thread::spawn(move || {
        let (_, context) = runtime();
        context.compile_image(source, "threads.js").unwrap()
    })
    .join()
    .unwrap();
    let images: Vec<_> = (0..3)
        .map(|_| {
            let image = image.clone();
            std::thread::spawn(move || {
                let before = front_end_parses();
                let (_, context) = runtime();
                let script = context.load_image(&image).unwrap();
                let result = printed(&context, context.execute(&script));
                (front_end_parses() - before, result)
            })
        })
        .collect();
    for handle in images {
        let (parses, result) = handle.join().unwrap();
        assert_eq!(parses, 0);
        assert_eq!(result, "total 9900");
    }
}

#[test]
fn an_image_counts_its_units() {
    let (_, context) = runtime();
    let script = context
        .compile("function a() { return () => 1; } a")
        .unwrap();
    let image = context.encode_image(&script).unwrap();
    assert_eq!(image.unit_count(), 3);
    assert!(image.bytes() > 0);
    // Only a compiled script reaches `encode_image` here; the core's encoder
    // refuses a closure or any other object (`only_a_script_encodes` in
    // `api::core::image`).
}

/// A script with every part the corruptions break: several string keys,
/// children, constants and a generator.
const RICH: &str =
    "var o = { alpha: 1, beta: 2 }; function* g(a, b) { yield o.alpha + a; yield o.beta + b; }
                    function f(x) { return [...g(x, 'k')].join('|') + 1.5; } f(o.alpha)";

#[test]
fn a_malformed_image_fails_to_load_and_changes_nothing() {
    let (_, compiled) = runtime();
    let image = compiled.compile_image(RICH, "rich.js").unwrap();
    for which in [
        "key-operand",
        "key-kind",
        "atom-index",
        "child-index",
        "child-twice",
        "constant-count",
        "constant-bits",
        "ic-count",
        "root-captures",
        "root-flags",
        "yield-flag",
        "register",
    ] {
        let broken = image
            .corrupted(which)
            .unwrap_or_else(|| panic!("{which}: nothing to break"));
        let (runtime, context) = runtime();
        let before = runtime.memory_report().unwrap();
        let result = context.load_image(&broken);
        assert!(
            matches!(result, Err(RuntimeError::Internal(_))),
            "{which}: {result:?}"
        );
        let after = runtime.memory_report().unwrap();
        assert_eq!(after.code.static_units, before.code.static_units, "{which}");
        assert_eq!(after.ic_entries, before.ic_entries, "{which}");
        assert_eq!(after.atoms, before.atoms, "{which}");
        assert_eq!(after.pinned_atoms, before.pinned_atoms, "{which}");
        // The intact image still loads into that runtime.
        let script = context.load_image(&image).unwrap();
        assert_eq!(printed(&context, context.execute(&script)), "2|2k1.5");
    }
}

#[test]
fn a_script_encodes_after_it_ran() {
    let (_, compiled) = runtime();
    let script = compiled.compile_with_filename(RICH, "ran.js").unwrap();
    assert_eq!(printed(&compiled, compiled.execute(&script)), "2|2k1.5");
    // Running mutated `o`; the code and its constants did not change.
    compiled.eval("o.alpha = 100").unwrap();
    let image = compiled.encode_image(&script).unwrap();
    let (_, loaded) = runtime();
    let script = loaded.load_image(&image).unwrap();
    assert_eq!(printed(&loaded, loaded.execute(&script)), "2|2k1.5");
}

#[test]
fn a_host_function_and_a_lend_load_images() {
    let (_, compiled) = runtime();
    let image = compiled.compile_image(RICH, "nested.js").unwrap();
    let (runtime, context) = runtime();
    let global = context.global_object().unwrap();
    let in_scope = image.clone();
    context
        .define_function(&global, "loadHere", 0, move |scope: &mut Scope<'_>| {
            let script = scope.load_image(&in_scope)?;
            scope.execute(&script)
        })
        .unwrap();
    let nested = context.clone();
    context
        .define_function(&global, "loadLent", 0, move |scope: &mut Scope<'_>| {
            scope.lend(|| {
                let script = nested.load_image(&image)?;
                nested.execute(&script)
            })
        })
        .unwrap();
    assert_eq!(
        printed(&context, context.eval("loadHere() + ' ' + loadLent()")),
        "2|2k1.5 2|2k1.5"
    );
    runtime.run_gc().unwrap();
    // The loaded functions outlive the host functions and the collection.
    assert_eq!(printed(&context, context.eval("f(1)")), "2|2k1.5");
}

#[test]
fn a_second_thread_keeps_source_text_and_filenames() {
    let source = "function shown(a, b) { return a + b; }\nfunction thrower() {\n  throw new Error('here');\n}\n[shown.toString(), (() => { try { thrower(); } catch (e) { return e.stack; } })()].join('#')";
    let (_, compiled) = runtime();
    let script = compiled
        .compile_with_filename(source, "threaded.js")
        .unwrap();
    let expected = printed(&compiled, compiled.execute(&script));
    assert!(
        expected.contains("function shown(a, b) { return a + b; }"),
        "{expected}"
    );
    let image = compiled.encode_image(&script).unwrap();
    let actual = std::thread::spawn(move || {
        let (_, context) = runtime();
        let script = context.load_image(&image).unwrap();
        printed(&context, context.execute(&script))
    })
    .join()
    .unwrap();
    assert!(actual.contains("threaded.js:3:"), "{actual}");
    assert_eq!(actual, expected);
}

#[test]
fn loads_share_the_units_bodies() {
    let mut source = String::new();
    for index in 0..50 {
        source.push_str(&format!(
            "function f{index}(a) {{ try {{ return a.b{index}.c; }} catch (e) {{ return 'caught {index} ' + e.message; }} }}\n"
        ));
    }
    source.push_str("f49({})");
    let (_, compiled) = runtime();
    let image = compiled.compile_image(&source, "shared.js").unwrap();
    let (first, one) = runtime();
    let (second, two) = runtime();
    for context in [&one, &two] {
        let script = context.load_image(&image).unwrap();
        assert_eq!(
            printed(context, context.execute(&script)),
            "caught 49 cannot read property 'c' of undefined"
        );
    }
    let one_report = first.memory_report().unwrap().code;
    let two_report = second.memory_report().unwrap().code;
    // Each runtime holds its instructions; the handlers, positions and
    // source text are the image's, held once.
    assert!(one_report.shared_body_bytes > 0);
    assert_eq!(one_report.shared_body_bytes, two_report.shared_body_bytes);
    assert!(
        one_report.static_bytes < one_report.shared_body_bytes,
        "{one_report:?}"
    );
    // A compiled script's bodies are its own until an image shares them.
    let (third, three) = runtime();
    let script = three.compile(&source).unwrap();
    assert_eq!(third.memory_report().unwrap().code.shared_body_bytes, 0);
    let _image = three.encode_image(&script).unwrap();
    assert!(third.memory_report().unwrap().code.shared_body_bytes > 0);
}

/// Cache entries come with a unit's first function object: a loaded
/// generator, async function or async generator first made and entered in
/// a second runtime gets its own, and a unit without sites needs none.
#[test]
fn a_loaded_runtime_reserves_caches_as_functions_are_made() {
    let source = "var o = { n: 1 };
                  function* gen() { yield o.n; yield o.n + 1; }
                  async function later() { await null; return o.n + 10; }
                  async function* agen() { yield o.n + 20; }
                  function empty() {}
                  globalThis.make = () => [...gen()].join() + ':' + typeof later() + ':' + typeof agen().next() + ':' + empty();";
    let (_, compiled) = runtime();
    let image = compiled.compile_image(source, "lazy.js").unwrap();
    let (first, one) = runtime();
    let (second, two) = runtime();
    for (runtime, context) in [(&first, &one), (&second, &two)] {
        let before = runtime.memory_report().unwrap().ic_entries;
        let script = context.load_image(&image).unwrap();
        let loaded = runtime.memory_report().unwrap().ic_entries;
        // Only the script's root is made by the load.
        assert!(loaded >= before);
        context.execute(&script).unwrap();
        assert_eq!(
            printed(context, context.eval("make()")),
            "1,2:object:object:undefined"
        );
        assert!(runtime.memory_report().unwrap().ic_entries > loaded);
        runtime.run_jobs().unwrap();
        assert_eq!(
            printed(
                context,
                context.eval("later().then(v => globalThis.got = v); 0")
            ),
            "0"
        );
        runtime.run_jobs().unwrap();
        assert_eq!(printed(context, context.eval("got")), "11");
    }
}

/// One image loaded by runtimes on eight threads at once: each runs it as
/// compiled, and the image's keys, names and string constants are the
/// process's shared atoms in every one of them.
#[test]
fn an_image_loaded_on_threads_at_once_shares_its_atoms() {
    let source = "var shapes = []; for (let i = 0; i < 20; i++) \
                  shapes.push({ imageSharedKey: i, imageSharedName: 'image-shared-constant-' + i }); \
                  function imageSharedFunction(o) { return o.imageSharedKey + o.imageSharedName.length; } \
                  shapes.map(imageSharedFunction).join() + '|' + JSON.stringify({ imageSharedJson: 'image-shared-text' })";
    let (_, compiled) = runtime();
    let expected = printed(&compiled, compiled.eval(source));
    let image = compiled.compile_image(source, "<threads>").unwrap();
    let results = std::thread::scope(|scope| {
        let handles = (0..8)
            .map(|_| {
                let image = image.clone();
                scope.spawn(move || {
                    let (runtime, loaded) = runtime();
                    let before = runtime.atom_census().unwrap();
                    let script = loaded.load_image(&image).unwrap();
                    let actual = printed(&loaded, loaded.execute(&script));
                    // A key read at run time finds the image's atom.
                    let key = printed(
                        &loaded,
                        loaded
                            .eval("Object.keys(shapes[0]).join() + ':' + imageSharedFunction.name"),
                    );
                    let after = runtime.atom_census().unwrap();
                    (
                        actual,
                        key,
                        after.shared - before.shared,
                        after.total - after.shared,
                    )
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    for (actual, key, shared, _) in &results {
        assert_eq!(actual, &expected);
        assert_eq!(key, "imageSharedKey,imageSharedName:imageSharedFunction");
        assert!(*shared >= 5, "the image's atoms are shared: {shared}");
    }
}

/// Runtimes on threads load one image at once, half of them after making
/// their own atom for one of its keys: each runs it as compiled and reads
/// that key, a runtime with an atom of its own builds its own key table for
/// the units that name it, and the others share the image's tables.
#[test]
fn an_image_shares_its_key_tables_except_where_a_runtime_had_its_own_atom() {
    let source = "function keyTableReader(o) { return o.keyTableFirst + o.keyTableSecond.length; } \
                  function keyTableOther(o) { return o.keyTableThird; } \
                  var made = { keyTableFirst: 2, keyTableSecond: 'abc' }; \
                  keyTableReader(made) + keyTableOther({ keyTableThird: 4 })";
    let (_, compiled) = runtime();
    let expected = printed(&compiled, compiled.eval(source));
    assert_eq!(expected, "9");
    let image = compiled.compile_image(source, "<key tables>").unwrap();
    // The odd threads make their atoms before any load shares the texts.
    let made = std::sync::Barrier::new(8);
    let results = std::thread::scope(|scope| {
        let handles = (0..8)
            .map(|thread| {
                let image = image.clone();
                let made = &made;
                scope.spawn(move || {
                    let (runtime, loaded) = runtime();
                    if thread % 2 == 1 {
                        // Its own atom for a key, made before the load.
                        let key = ["keyTable", "Second"].concat();
                        let own = format!("var own = {{}}; own['{key}'] = 1; own.{key}");
                        assert_eq!(printed(&loaded, loaded.eval(&own)), "1");
                    }
                    made.wait();
                    let script = loaded.load_image(&image).unwrap();
                    let actual = printed(&loaded, loaded.execute(&script));
                    let again = printed(
                        &loaded,
                        loaded.eval("keyTableReader({ keyTableFirst: 5, keyTableSecond: 'xy' })"),
                    );
                    let census = runtime.atom_census().unwrap();
                    (thread, actual, again, census.own_key_entries)
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    for (thread, actual, again, own) in results {
        assert_eq!(actual, expected, "thread {thread}");
        assert_eq!(again, "7", "thread {thread}");
        if thread % 2 == 0 {
            assert_eq!(own, 0, "thread {thread} shares every image key table");
        } else {
            assert!(
                own > 0,
                "thread {thread} has its own table where its atom came first"
            );
        }
    }
}
