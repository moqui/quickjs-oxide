//! Code images: compile a script once, load it into other runtimes without
//! parsing, and compare what each costs.
//!
//! ```sh
//! cargo run --quiet --release --example code_images [script.js]
//! ```
//!
//! Without an argument it uses a generated script of 400 functions.

use std::time::Instant;

use quickjs_oxide::engine::api::core::{Context, MemoryReport, Runtime};

/// What a runtime holds: its heap's reserved bytes and its code store's.
fn held(report: &MemoryReport) -> usize {
    report.reserved_bytes() + report.code.static_bytes
}

fn cost(context: &Context, run: impl FnOnce(&Context)) -> (usize, f64) {
    let runtime = context.runtime();
    let before = held(&runtime.memory_report().unwrap());
    let start = Instant::now();
    run(context);
    let seconds = start.elapsed().as_secs_f64();
    let after = held(&runtime.memory_report().unwrap());
    (after.saturating_sub(before), seconds)
}

fn generated() -> String {
    let mut source = String::new();
    for index in 0..400 {
        source.push_str(&format!(
            "function f{index}(a, b) {{ const o = {{ name: 'f{index}', a, b, list: [a, b, {index}] }};\n\
             if (o.a > o.b) {{ return o.list.map(x => x * 2).join(','); }}\n\
             return `${{o.name}}:${{o.a + o.b}}`; }}\n"
        ));
    }
    source
}

fn main() {
    let (source, name) = match std::env::args().nth(1) {
        Some(path) => (std::fs::read_to_string(&path).unwrap(), path),
        None => (generated(), "generated.js".to_string()),
    };
    println!("{name}: {} bytes of source", source.len());

    let compiling = Runtime::new().context();
    let mut script = None;
    let (compile_bytes, compile_seconds) = cost(&compiling, |context| {
        script = Some(context.compile_with_filename(&source, &name).unwrap());
    });
    let image = compiling.encode_image(&script.unwrap()).unwrap();
    println!(
        "image: {} units, about {} KB",
        image.unit_count(),
        image.bytes() / 1024
    );
    println!(
        "compile: {:.0} µs, the runtime holds {} KB more",
        compile_seconds * 1e6,
        compile_bytes / 1024
    );

    const LOADS: usize = 20;
    let mut load_seconds = 0.0;
    let mut load_bytes = 0;
    for _ in 0..LOADS {
        let loading = Runtime::new().context();
        let (bytes, seconds) = cost(&loading, |context| {
            context.load_image(&image).unwrap();
        });
        load_seconds += seconds;
        load_bytes = bytes;
    }
    let load_seconds = load_seconds / LOADS as f64;
    println!(
        "load: {:.0} µs ({:.1}x faster than compiling), the runtime holds {} KB more",
        load_seconds * 1e6,
        compile_seconds / load_seconds,
        load_bytes / 1024
    );
}
