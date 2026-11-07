//! Code images: a compiled script by value, loaded into any runtime of the
//! same build without parsing or compiling it again.

use super::RuntimeError;
use super::context::FunctionBytecodeRef;
use super::ops::{JsResult, View};

/// A compiled script's code by value: its units, with every atom by text
/// and every constant by value, naming nothing of the runtime that made
/// it. `Send + Sync`; cloning shares it, so a process keeps one per script
/// text and each thread's runtime loads it
/// ([`Js::load_image`](super::Js::load_image)) instead of compiling.
#[derive(Clone, Debug)]
pub struct CodeImage(pub(crate) oxide_core::CodeImage);

impl CodeImage {
    /// The number of functions in the script, the script's own included.
    pub fn unit_count(&self) -> usize {
        self.0.unit_count()
    }

    /// Bytes the image holds, approximately.
    pub fn bytes(&self) -> usize {
        self.0.bytes()
    }

    /// A copy with one part broken, for the loader's tests
    /// (`oxide_core::CodeImage::corrupted` names the parts).
    #[doc(hidden)]
    pub fn corrupted(&self, which: &str) -> Option<CodeImage> {
        self.0.corrupted(which).map(CodeImage)
    }
}

/// The scripts, modules and `eval` sources this thread's front end has
/// parsed so far: what a test reads to show that loading an image parses
/// nothing.
#[doc(hidden)]
pub fn front_end_parses() -> u64 {
    crate::engine::compiler::parses()
}

fn load_error(error: oxide_core::LoadError) -> RuntimeError {
    match error {
        oxide_core::LoadError::Heap(oxide_core::HeapError::OutOfMemory) => {
            RuntimeError::OutOfMemory
        }
        other => RuntimeError::Internal(other.to_string()),
    }
}

impl View<'_> {
    pub(crate) fn encode_image(&mut self, script: &FunctionBytecodeRef) -> JsResult<CodeImage> {
        let function = self.object(script.function())?;
        match self.core.encode_image(function) {
            Ok(image) => Ok(CodeImage(image)),
            Err(oxide_core::EncodeError::NotBytecode) => {
                Err(RuntimeError::Invariant("a compiled script"))
            }
            Err(other) => Err(RuntimeError::Internal(other.to_string())),
        }
    }

    pub(crate) fn compile_image(&mut self, source: &str, filename: &str) -> JsResult<CodeImage> {
        let options = crate::engine::compiler::CompileOptions {
            filename: filename.to_owned(),
            transient: true,
        };
        let script = self.compile_with_options(source, &options)?;
        self.encode_image(&script)
    }

    pub(crate) fn load_image(&mut self, image: &CodeImage) -> JsResult<FunctionBytecodeRef> {
        let function = self.core.load_image(&image.0).map_err(load_error)?;
        Ok(FunctionBytecodeRef::loaded(self.lift_object(function)))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Runtime, Value};

    #[test]
    fn an_image_is_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<super::CodeImage>();
    }

    /// Only a script's root encodes: a closure's cells, a plain function's
    /// unit and any other object are refused, never encoded.
    #[test]
    fn only_a_script_encodes() {
        let runtime = Runtime::new();
        let context = runtime.context();
        for source in [
            "(function () { let x = 1; return () => x; })()",
            "(function plain(a) { return a; })",
            "({})",
            "[1, 2]",
            "Math.max",
        ] {
            let Value::Object(object) = context.eval(source).unwrap() else {
                panic!("{source}: not an object")
            };
            let mut entry = runtime.enter().unwrap();
            let mut view = entry.view();
            let id = view.object(&object).unwrap();
            assert_eq!(
                view.core.encode_image(id).map(drop),
                Err(oxide_core::EncodeError::NotBytecode),
                "{source}"
            );
        }
    }
}
