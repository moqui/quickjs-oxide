//! `using` and `await using` declarations, a fork addition (deviation
//! ledger, `FORK-EXPLICIT-RESOURCE-MANAGEMENT-001`).
//!
//! A scope's first declaration opens its dispose region: a new dispose
//! capability in a synthetic local, then a catch marker whose control is a
//! `TryFinally`, so break, continue and return leave through the region's
//! finally subroutine exactly as they leave a `try` with a `finally`. The
//! scope's exit closes the region in the shape of `try … finally`: the
//! normal path calls the subroutine, the handler records the throw in the
//! capability and calls it, and the subroutine disposes the resources
//! (`DisposeResources`), throwing the completion when it is a throw. A
//! region holding an `await using` resource awaits each disposal in the
//! subroutine with the ordinary `Await`, one `DisposeStep` at a time.
//!
//! Control enters a statement list only at its top, so the region can open
//! at the declaration; `using` directly in a case clause is an early error.

use crate::engine::api::error::Error;
use crate::engine::code::bytecode::Instruction;
use crate::engine::code::function::metadata::FunctionKind as BytecodeFunctionKind;
use crate::engine::compiler::lexer::Keyword;
use crate::engine::compiler::lexer::LexicalGoal;
use crate::engine::compiler::lexer::Punctuator;
use crate::engine::compiler::lexer::TokenKind;
use crate::engine::compiler::model::bindings::SyntheticLocalKind;
use crate::engine::compiler::model::ir::IdentifierAccess;
use crate::engine::compiler::model::ir::IrConstant;
use crate::engine::compiler::model::scope::ScopeId;
use crate::engine::compiler::parser::context::BreakControlKind;
use crate::engine::compiler::parser::context::DisposeRegion;
use crate::engine::compiler::parser::context::Parser;
use crate::engine::compiler::parser::diagnostics::IdentifierContext;
use crate::engine::compiler::parser::diagnostics::source_offset;
use crate::engine::value::JsString;
use crate::engine::value::PrimitiveValue as Value;

impl<'source> Parser<'source> {
    /// Whether a `using` (`Some(false)`) or `await using` (`Some(true)`)
    /// declaration starts here: `using` and then, on the same line, a
    /// binding identifier; `await` (the keyword, in an async body) before
    /// it, on the same line. In a for-in/of head (`for_head`), `using of`
    /// stays the identifier `using`.
    pub(in crate::engine::compiler) fn using_declaration_ahead(
        &self,
        for_head: bool,
    ) -> Result<Option<bool>, Error> {
        let is_async = match &self.current().kind {
            TokenKind::Identifier(identifier) if self.is_unescaped_name(identifier, "using") => {
                false
            }
            TokenKind::Keyword(Keyword::Await)
                if matches!(
                    self.current_ir().execution_kind,
                    BytecodeFunctionKind::Async | BytecodeFunctionKind::AsyncGenerator
                ) && self.current_ir().context.in_function_body =>
            {
                true
            }
            _ => return Ok(None),
        };
        let mut lexer = self.lexer.clone();
        lexer.seek(self.current().span.end);
        if is_async {
            let Ok(using) = self.probe_token(&mut lexer, LexicalGoal::Div) else {
                return Ok(None);
            };
            if using.line_terminator_before
                || !matches!(&using.kind, TokenKind::Identifier(identifier)
                    if self.is_unescaped_name(identifier, "using"))
            {
                return Ok(None);
            }
        }
        // A token that does not lex is left to the real parser's goal and
        // diagnostic.
        let Ok(name) = self.probe_token(&mut lexer, LexicalGoal::Div) else {
            return Ok(None);
        };
        if name.line_terminator_before {
            return Ok(None);
        }
        let TokenKind::Identifier(identifier) = &name.kind else {
            return Ok(None);
        };
        if for_head && !is_async && self.is_unescaped_name(identifier, "of") {
            return Ok(None);
        }
        Ok(Some(is_async))
    }

    /// A `using` or `await using` declaration statement in a statement list.
    pub(in crate::engine::compiler) fn parse_using_statement(
        &mut self,
        is_async: bool,
    ) -> Result<(), Error> {
        self.parse_using_declarations(is_async)?;
        self.consume_statement_terminator()
    }

    /// `using BindingList` (or `await using BindingList`) with the current
    /// `in` mode: each binding is initialized, then its value is added to
    /// the scope's dispose capability before the binding is.
    pub(in crate::engine::compiler) fn parse_using_declarations(
        &mut self,
        is_async: bool,
    ) -> Result<(), Error> {
        if is_async {
            self.advance()?;
        }
        self.advance()?;
        let local = self.open_dispose_region(is_async)?;
        loop {
            let name_span = self.current().span;
            let name = self.parse_using_binding_identifier()?;
            let initializer_span = self.current().span;
            if !self.consume_punctuator(Punctuator::Equal)? {
                return Err(Error::syntax(
                    "missing initializer for using declaration",
                    self.current().span,
                ));
            }
            let site = source_offset(initializer_span)?;
            self.parse_assignment()?;
            if let Some(definition) = self.take_anonymous_function_definition() {
                let name_constant = self.add_constant(IrConstant::Primitive(Value::String(
                    JsString::try_from_utf8(self.names.name(name))?,
                )))?;
                self.emit_anonymous_set_name(definition, Instruction::SetName(name_constant))?;
            }
            self.emit_instruction(Instruction::GetLocal(local))?;
            self.emit_instruction(Instruction::DisposeAdd(is_async))?;
            self.emit_identifier_at(name, name_span, IdentifierAccess::Initialize, site)?;
            if !self.consume_punctuator(Punctuator::Comma)? {
                break;
            }
        }
        Ok(())
    }

    /// The BindingIdentifier of a `using` binding, registered as a constant
    /// lexical binding of the current scope. Binding patterns are early
    /// errors.
    pub(in crate::engine::compiler) fn parse_using_binding_identifier(
        &mut self,
    ) -> Result<crate::engine::compiler::names::NameId, Error> {
        let token = *self.current();
        let TokenKind::Identifier(identifier) = token.kind else {
            return Err(self.syntax_here("variable name expected"));
        };
        self.validate_identifier_reservation(
            &identifier,
            token.span,
            self.current_ir().strict,
            IdentifierContext::Variable,
        )?;
        if self.identical_name(&identifier, "let") {
            return Err(Error::syntax(
                "'let' is not a valid lexical identifier",
                token.span,
            ));
        }
        let name = self.intern_identifier(&identifier);
        let strict = self.current_ir().strict;
        self.advance()?;
        if strict && matches!(self.names.name(name), "eval" | "arguments") {
            return Err(Error::syntax(
                "invalid variable name in strict mode",
                self.current().span,
            ));
        }
        self.register_lexical_binding(name, token.span, self.current().span, true, false)?;
        Ok(name)
    }

    /// The `for (using x of …)` head binding, in the assignment fragment
    /// with the iterated value on the stack: each iteration gets a new
    /// capability, then the value is added and the binding initialized. The
    /// region's catch marker follows the fragment
    /// ([`Self::enter_for_of_dispose_region`]).
    pub(in crate::engine::compiler) fn parse_for_of_using_binding(
        &mut self,
        is_async: bool,
    ) -> Result<(), Error> {
        if is_async {
            self.advance()?;
        }
        self.advance()?;
        let name_span = self.current().span;
        let name = self.parse_using_binding_identifier()?;
        let local = self.begin_dispose_region(is_async)?;
        self.emit_instruction(Instruction::GetLocal(local))?;
        self.emit_instruction(Instruction::DisposeAdd(is_async))?;
        self.emit_identifier_at(
            name,
            name_span,
            IdentifierAccess::Initialize,
            source_offset(name_span)?,
        )?;
        Ok(())
    }

    /// The local of the current scope's dispose region, opening the region
    /// at its first declaration.
    fn open_dispose_region(&mut self, is_async: bool) -> Result<u16, Error> {
        let scope = self.current_ir().context.current_scope;
        if let Some(region) = self.current_ir_mut().context.dispose_regions.last_mut()
            && region.scope == scope
        {
            region.is_async |= is_async;
            return Ok(region.local);
        }
        let local = self.begin_dispose_region(is_async)?;
        self.emit_dispose_catch()?;
        self.push_dispose_control()?;
        Ok(local)
    }

    /// A new region of the current scope: a fresh capability in its local.
    fn begin_dispose_region(&mut self, is_async: bool) -> Result<u16, Error> {
        let function = &mut self.functions[self.current_function];
        let local =
            function.add_synthetic_local(SyntheticLocalKind::DisposeCapability, &mut self.names)?;
        self.emit_instruction(Instruction::DisposeNew)?;
        self.emit_instruction(Instruction::PutLocal(local))?;
        let context = &mut self.current_ir_mut().context;
        let scope = context.current_scope;
        let entry_depth = context.stack_depth;
        context.dispose_regions.push(DisposeRegion {
            scope,
            local,
            catch_jump: None,
            entry_depth,
            is_async,
        });
        Ok(local)
    }

    fn emit_dispose_catch(&mut self) -> Result<(), Error> {
        let entry_depth = self.current_ir().context.stack_depth;
        let catch_jump = self.emit_instruction(Instruction::Catch(u32::MAX))?;
        let region = self
            .current_ir_mut()
            .context
            .dispose_regions
            .last_mut()
            .ok_or_else(|| Error::internal("dispose region disappeared"))?;
        region.catch_jump = Some(catch_jump);
        region.entry_depth = entry_depth;
        Ok(())
    }

    fn push_dispose_control(&mut self) -> Result<(), Error> {
        let entry_depth = self
            .current_ir()
            .context
            .dispose_regions
            .last()
            .ok_or_else(|| Error::internal("dispose region disappeared"))?
            .entry_depth;
        self.push_break_control(BreakControlKind::TryFinally, None, entry_depth + 1, 1);
        Ok(())
    }

    /// Whether the innermost region is the head region of the for-of scope
    /// `scope`, its catch marker not yet emitted.
    pub(in crate::engine::compiler) fn pending_for_of_dispose_region(
        &self,
        scope: ScopeId,
    ) -> bool {
        self.current_ir()
            .context
            .dispose_regions
            .last()
            .is_some_and(|region| region.scope == scope && region.catch_jump.is_none())
    }

    /// After a for-of head's assignment fragment: the catch marker, which
    /// stays on the stack through the body.
    pub(in crate::engine::compiler) fn enter_for_of_dispose_region(&mut self) -> Result<(), Error> {
        self.emit_dispose_catch()
    }

    /// Before a for-of body: the region's `TryFinally` control.
    pub(in crate::engine::compiler) fn push_for_of_dispose_control(&mut self) -> Result<(), Error> {
        self.push_dispose_control()
    }

    /// Whether `scope` has an open dispose region.
    pub(in crate::engine::compiler) fn has_dispose_region(&self, scope: ScopeId) -> bool {
        self.current_ir()
            .context
            .dispose_regions
            .last()
            .is_some_and(|region| region.scope == scope)
    }

    /// Closes the dispose region of `scope`, if it has one, at the scope's
    /// normal exit.
    pub(in crate::engine::compiler) fn close_dispose_region(
        &mut self,
        scope: ScopeId,
    ) -> Result<(), Error> {
        if !self.has_dispose_region(scope) {
            return Ok(());
        }
        let region = self
            .current_ir_mut()
            .context
            .dispose_regions
            .pop()
            .ok_or_else(|| Error::internal("dispose region disappeared"))?;
        let catch_jump = region
            .catch_jump
            .ok_or_else(|| Error::internal("dispose region has no catch marker"))?;
        let entry_depth = region.entry_depth;
        self.require_stack_depth(entry_depth + 1, "using scope")?;
        let control = self.pop_break_control()?;
        if control.kind != BreakControlKind::TryFinally {
            return Err(Error::internal("using scope lost its finally control"));
        }
        let mut gosubs = control.finally_gosubs;

        // Normal completion.
        self.emit_instruction(Instruction::DropCatch)?;
        self.emit_instruction(Instruction::Undefined)?;
        gosubs.push(self.emit_instruction(Instruction::Gosub(u32::MAX))?);
        self.emit_instruction(Instruction::Drop)?;
        let end_jump = self.emit_instruction(Instruction::Goto(u32::MAX))?;

        // Throw completion: the thrown value where the marker was. The
        // subroutine throws the recorded completion, so the rethrow after it
        // is never reached.
        self.current_ir_mut().context.stack_depth = entry_depth + 1;
        let handler = self.current_ir().ops.len();
        self.patch_jump(catch_jump, handler)?;
        self.emit_instruction(Instruction::Dup)?;
        self.emit_instruction(Instruction::GetLocal(region.local))?;
        self.emit_instruction(Instruction::DisposeRecordError)?;
        gosubs.push(self.emit_instruction(Instruction::Gosub(u32::MAX))?);
        self.emit_instruction(Instruction::Throw)?;

        // The finally subroutine, entered with a pending value and the
        // return address.
        let subroutine = self.current_ir().ops.len();
        for gosub in gosubs {
            self.patch_jump(gosub, subroutine)?;
        }
        let depth = entry_depth + 2;
        self.current_ir_mut().context.stack_depth = depth;
        if region.is_async {
            self.emit_async_disposal(region.local, depth)?;
        } else {
            self.emit_instruction(Instruction::GetLocal(region.local))?;
            self.emit_instruction(Instruction::DisposeSync)?;
        }
        self.require_stack_depth(depth, "using scope disposal")?;
        self.emit_instruction(Instruction::Ret)?;

        let end = self.current_ir().ops.len();
        self.patch_jump(end_jump, end)?;
        self.current_ir_mut().context.stack_depth = entry_depth;
        Ok(())
    }

    /// `DisposeResources` with its awaits: each step's value is awaited
    /// under a catch marker whose handler records the throw.
    fn emit_async_disposal(&mut self, local: u16, depth: usize) -> Result<(), Error> {
        let to_jump = |target: usize| {
            u32::try_from(target).map_err(|_| Error::internal("using disposal target overflow"))
        };
        let top = self.current_ir().ops.len();
        let catch_jump = self.emit_instruction(Instruction::Catch(u32::MAX))?;
        self.emit_instruction(Instruction::GetLocal(local))?;
        self.emit_instruction(Instruction::DisposeStep)?;
        let done_jump = self.emit_instruction(Instruction::IfFalse(u32::MAX))?;
        self.emit_instruction(Instruction::Await)?;
        self.emit_instruction(Instruction::Drop)?;
        self.emit_instruction(Instruction::DropCatch)?;
        self.emit_instruction(Instruction::Goto(to_jump(top)?))?;

        // Every resource is disposed: the marker and the unused value.
        self.current_ir_mut().context.stack_depth = depth + 2;
        let done = self.current_ir().ops.len();
        self.patch_jump(done_jump, done)?;
        self.emit_instruction(Instruction::Drop)?;
        self.emit_instruction(Instruction::DropCatch)?;
        self.emit_instruction(Instruction::GetLocal(local))?;
        self.emit_instruction(Instruction::DisposeFinish)?;
        let finished_jump = self.emit_instruction(Instruction::Goto(u32::MAX))?;

        // The awaited disposal threw.
        self.current_ir_mut().context.stack_depth = depth + 1;
        let handler = self.current_ir().ops.len();
        self.patch_jump(catch_jump, handler)?;
        self.emit_instruction(Instruction::GetLocal(local))?;
        self.emit_instruction(Instruction::DisposeRecordError)?;
        self.emit_instruction(Instruction::Goto(to_jump(top)?))?;

        self.current_ir_mut().context.stack_depth = depth;
        let finished = self.current_ir().ops.len();
        self.patch_jump(finished_jump, finished)?;
        Ok(())
    }
}
