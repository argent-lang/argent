//! Source reference bindings, including lexical scopes in the current hybrid AST.
//! Spans always refer to authored text. No replacement text is produced here.

use super::*;
use crate::compiler::syntax::body::{EntryBinding, EntryStatement, EntrySuccessor};
use crate::compiler::syntax::lexer::{Span, Token, TokenKind, lex};
use silverscript_lang::parser::{Rule, parse_expression};

/// Identifies a text-bearing node within its owning declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TextSite {
    Value,
    Function(usize),
    Entry(usize),
    Observe { entry: usize, observe: usize },
}

/// Identifies an actor target in an observe or spawn clause.
/// Targets are resolved per occurrence because local actor bindings can shadow declarations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ActorSite {
    ObserveInput { entry: usize, observe: usize, input: usize },
    ObserveOutput { entry: usize, observe: usize, output: usize },
    Spawn { entry: usize, spawn: usize, output: usize },
}

#[derive(Debug, Clone)]
pub(crate) struct BoundReference {
    pub span: Span,
    pub target: ResolvedName,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DeclarationBindings {
    pub names: BTreeMap<String, ResolvedName>,
    pub text: BTreeMap<TextSite, Vec<BoundReference>>,
    pub actor_targets: BTreeMap<ActorSite, ResolvedName>,
    pub local_actor_targets: BTreeSet<ActorSite>,
    /// Names left to local resolution. Foreign declarations must not capture them.
    pub unbound_names: BTreeSet<String>,
    pub declarations: BTreeSet<DeclId>,
    pub apps: BTreeSet<AppMember>,
}

struct DeclarationBinder<'a> {
    program: &'a ResolvedModules,
    owner: DeclId,
    bindings: DeclarationBindings,
    actor_fields: BTreeSet<String>,
    /// Track shadowing so earlier target validation preserves entry diagnostics.
    entry_parameters: BTreeMap<String, bool>,
}

impl DeclarationBindings {
    pub(super) fn resolve(program: &ResolvedModules, owner: DeclId) -> Result<Self> {
        let mut binder = DeclarationBinder {
            program,
            owner,
            bindings: Self::default(),
            actor_fields: BTreeSet::new(),
            entry_parameters: BTreeMap::new(),
        };
        match program.declaration(owner) {
            ResolvedDeclaration::Const(item) => {
                binder.ty(&item.ty)?;
                let tokens = lex(&item.value)?;
                let references = binder.references(&item.value, &tokens, &BTreeSet::new(), &[])?;
                binder.bindings.text.insert(TextSite::Value, references);
            }
            ResolvedDeclaration::State(item) => {
                for field in &item.fields {
                    binder.ty(&field.ty)?;
                }
                if let Some(expansion) = &item.expansion {
                    binder.required(&expansion.base, &[SymbolKind::State])?;
                    for digest in &expansion.digests {
                        binder.required(&digest.state, &[SymbolKind::State])?;
                    }
                }
            }
            ResolvedDeclaration::Function(item) => binder.function(0, item, &BTreeSet::new())?,
            ResolvedDeclaration::Actor(item) => {
                let state = binder.required(&item.state, &[SymbolKind::State])?;
                let mut scope = item.functions.iter().map(|function| function.name.clone()).collect::<BTreeSet<_>>();
                let mut pending = Some(state);
                let mut visited = BTreeSet::new();
                while let Some(id) = pending.take() {
                    if !visited.insert(id) {
                        break;
                    }
                    if let ResolvedDeclaration::State(state) = program.declaration(id) {
                        scope.extend(state.fields.iter().map(|field| field.name.clone()));
                        binder
                            .actor_fields
                            .extend(state.fields.iter().filter(|field| field.ty.is_actor_type()).map(|field| field.name.clone()));
                        if let Some(expansion) = &state.expansion
                            && let ResolvedName::Declaration(base) = program.resolve(id.module, &expansion.base)?
                        {
                            pending = Some(base);
                        }
                    }
                }
                for (index, function) in item.functions.iter().enumerate() {
                    binder.function(index, function, &scope)?;
                }
                for (index, entry) in item.entries.iter().enumerate() {
                    binder.entry(index, entry, &scope)?;
                }
            }
            ResolvedDeclaration::ActorEnum(item) => {
                for actor in &item.variants {
                    binder.required(actor, &[SymbolKind::Actor])?;
                }
            }
            ResolvedDeclaration::App(item) => {
                for actor in &item.actors {
                    binder.required(actor, &[SymbolKind::Actor])?;
                }
            }
        }
        Ok(binder.bindings)
    }
}

impl DeclarationBinder<'_> {
    fn record(&mut self, target: ResolvedName) {
        match target {
            ResolvedName::Declaration(id) => {
                self.bindings.declarations.insert(id);
            }
            ResolvedName::AppMember(member) => {
                self.bindings.apps.insert(member);
            }
            ResolvedName::Module(_) => {}
        }
    }

    fn required(&mut self, name: &str, expected: &[SymbolKind]) -> Result<DeclId> {
        match self.program.resolve(self.owner.module, name)? {
            ResolvedName::Declaration(id) if expected.contains(&id.kind) => {
                self.bindings.names.insert(name.to_string(), ResolvedName::Declaration(id));
                self.record(ResolvedName::Declaration(id));
                Ok(id)
            }
            ResolvedName::Declaration(id) => Err(self.program.wrong_kind(self.owner.module, name, expected, id.kind)),
            ResolvedName::AppMember(_) => Err(ArgentError::at(
                self.program.declaration_path(self.owner),
                format!("app member `{name}` cannot be used as a local declaration"),
            )),
            ResolvedName::Module(_) => Err(ArgentError::at(
                self.program.declaration_path(self.owner),
                format!("module namespace `{name}` does not name a declaration"),
            )),
        }
    }

    fn ty(&mut self, ty: &TypeRef) -> Result<()> {
        if let Some(state) = &ty.actor_state {
            self.required(state, &[SymbolKind::State])?;
        // built-in types do not need resolution
        } else if !ty.is_builtin() {
            self.required(&ty.name, &[SymbolKind::State, SymbolKind::ActorEnum])?;
        }
        Ok(())
    }

    fn cardinality(&mut self, cardinality: &Cardinality) -> Result<()> {
        if let Cardinality::Range { minimum, maximum } = cardinality {
            for bound in [minimum, maximum] {
                if let CardinalityBound::Const(name) = bound {
                    self.required(name, &[SymbolKind::Const])?;
                }
            }
        }
        Ok(())
    }

    fn actor_target(&mut self, site: ActorSite, name: &str, locals: &BTreeSet<String>) -> Result<()> {
        // Open observed actors and actor_type parameters are local bindings.
        if locals.contains(name) {
            self.bindings.local_actor_targets.insert(site);
            return Ok(());
        }
        match self.program.resolve(self.owner.module, name) {
            Ok(target @ ResolvedName::AppMember(_)) => {
                self.bindings.actor_targets.insert(site, target);
                self.record(target);
            }
            Ok(ResolvedName::Declaration(id)) if matches!(id.kind, SymbolKind::Actor | SymbolKind::ActorEnum) => {
                let target = ResolvedName::Declaration(id);
                self.bindings.actor_targets.insert(site, target);
                self.record(target);
            }
            Ok(ResolvedName::Declaration(id)) => {
                return Err(self.program.wrong_kind(self.owner.module, name, &[SymbolKind::Actor, SymbolKind::ActorEnum], id.kind));
            }
            Ok(ResolvedName::Module(_)) => {
                return Err(ArgentError::at(
                    self.program.declaration_path(self.owner),
                    format!("module namespace `{name}` cannot be used as an actor"),
                ));
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    fn function(&mut self, index: usize, function: &FunctionDecl, outer: &BTreeSet<String>) -> Result<()> {
        let mut scope = outer.clone();
        for param in &function.params {
            self.ty(&param.ty)?;
            scope.insert(param.name.clone());
        }
        if let Some(ty) = &function.return_ty {
            self.ty(ty)?;
        }
        self.bindings.unbound_names.extend(scope.iter().cloned());
        // Functions still store text; use the same structural parser as entries
        // until function bodies become Sil statement nodes in the source AST.
        let body = EntryBody::new(function.body.clone())?;
        let mut references = Vec::new();
        let mut actor_locals = BTreeSet::new();
        for statement in body.statements() {
            self.statement(&body, statement, &mut scope, &mut actor_locals, &mut references)?;
        }
        self.bindings.text.insert(TextSite::Function(index), references);
        Ok(())
    }

    fn entry(&mut self, index: usize, entry: &EntryDecl, outer: &BTreeSet<String>) -> Result<()> {
        self.entry_parameters = entry.params.iter().map(|param| (param.name.clone(), false)).collect();
        let mut scope = outer.clone();
        for param in &entry.params {
            self.ty(&param.ty)?;
            scope.insert(param.name.clone());
        }
        for consume in &entry.consumes {
            self.required(&consume.actor, &[SymbolKind::Actor])?;
            self.cardinality(&consume.cardinality)?;
            scope.insert(consume.name.clone());
        }
        for observe in &entry.observes {
            scope.insert(observe.name.clone());
            for actor in &observe.inputs {
                if actor.open_state.is_some() {
                    scope.insert(actor.actor.clone());
                }
            }
        }
        for spawn in &entry.spawns {
            scope.insert(spawn.name.clone());
            scope.insert(spawn.covenant.clone());
        }
        if let EmitSpec::Outputs(outputs) = &entry.emits {
            for output in outputs {
                for actor in &output.actors {
                    self.required(actor, &[SymbolKind::Actor, SymbolKind::ActorEnum])?;
                }
                self.cardinality(&output.cardinality)?;
                scope.insert(output.name.clone());
            }
        }
        let mut actor_locals = entry.params.iter().filter(|param| {
            param.ty.is_actor_type() || (param.ty.array.is_none() && matches!(self.program.resolve(self.owner.module, &param.ty.name), Ok(ResolvedName::Declaration(id)) if id.kind == SymbolKind::ActorEnum))
        }).map(|param| param.name.clone()).collect::<BTreeSet<_>>();
        actor_locals.extend(self.actor_fields.iter().flat_map(|field| [field.clone(), format!("self.{field}")]));
        for (observe_index, observe) in entry.observes.iter().enumerate() {
            let references = self.references(&observe.covenant_expr, &lex(&observe.covenant_expr)?, &scope, &[])?;
            self.bindings.text.insert(TextSite::Observe { entry: index, observe: observe_index }, references);
            let mut actor_scope = actor_locals.clone();
            actor_scope.extend(observe.inputs.iter().filter(|actor| actor.open_state.is_some()).map(|actor| actor.actor.clone()));
            let inputs = observe
                .inputs
                .iter()
                .enumerate()
                .map(|(input, actor)| (ActorSite::ObserveInput { entry: index, observe: observe_index, input }, actor));
            let outputs = observe
                .outputs
                .iter()
                .enumerate()
                .map(|(output, actor)| (ActorSite::ObserveOutput { entry: index, observe: observe_index, output }, actor));
            for (site, actor) in inputs.chain(outputs) {
                if let Some(state) = &actor.open_state {
                    self.required(state, &[SymbolKind::State])?;
                }
                self.actor_target(site, &actor.actor, &actor_scope)?;
                self.cardinality(&actor.cardinality)?;
            }
        }
        let actor_scope = actor_locals.clone();
        for (spawn_index, spawn) in entry.spawns.iter().enumerate() {
            for (output_index, output) in spawn.outputs.iter().enumerate() {
                self.actor_target(
                    ActorSite::Spawn { entry: index, spawn: spawn_index, output: output_index },
                    &output.actor,
                    &actor_scope,
                )?;
                self.cardinality(&output.cardinality)?;
            }
        }
        let mut references = Vec::new();
        self.bindings.unbound_names.extend(scope.iter().cloned());
        actor_locals.extend(
            entry
                .observes
                .iter()
                .flat_map(|observe| observe.inputs.iter())
                .filter(|actor| actor.open_state.is_some())
                .map(|actor| actor.actor.clone()),
        );
        for statement in entry.body.statements() {
            self.statement(&entry.body, statement, &mut scope, &mut actor_locals, &mut references)?;
        }
        self.bindings.text.insert(TextSite::Entry(index), references);
        Ok(())
    }

    fn statement(
        &mut self,
        body: &EntryBody,
        statement: &EntryStatement,
        locals: &mut BTreeSet<String>,
        actor_locals: &mut BTreeSet<String>,
        references: &mut Vec<BoundReference>,
    ) -> Result<()> {
        let introduced = match statement {
            EntryStatement::Local { declaration, .. } => std::slice::from_ref(&declaration.binding),
            EntryStatement::Plain { bindings, .. } => bindings.as_slice(),
            EntryStatement::For { binding, .. } => std::slice::from_ref(binding),
            _ => &[],
        };
        for binding in introduced {
            if let Some(shadowed) = self.entry_parameters.get_mut(&binding.name) {
                *shadowed = true;
            }
        }
        match statement {
            EntryStatement::Block { statements, .. } => {
                let mut scope = locals.clone();
                let mut actor_scope = actor_locals.clone();
                for statement in statements {
                    self.statement(body, statement, &mut scope, &mut actor_scope, references)?;
                }
            }
            EntryStatement::If { condition, then_branch, else_branch, .. } => {
                references.extend(self.span(body, *condition, locals, &[])?);
                self.statement(body, then_branch, &mut locals.clone(), &mut actor_locals.clone(), references)?;
                if let Some(branch) = else_branch {
                    self.statement(body, branch, &mut locals.clone(), &mut actor_locals.clone(), references)?;
                }
            }
            EntryStatement::For { binding, header, body: loop_body, .. } => {
                self.bindings.unbound_names.insert(binding.name.clone());
                references.extend(self.span(body, *header, locals, &[binding])?);
                let mut scope = locals.clone();
                scope.insert(binding.name.clone());
                let mut actor_scope = actor_locals.clone();
                actor_scope.remove(&binding.name);
                self.statement(body, loop_body, &mut scope, &mut actor_scope, references)?;
            }
            EntryStatement::Local { declaration, span } => {
                self.bindings.unbound_names.insert(declaration.binding.name.clone());
                references.extend(self.span(body, *span, locals, &[&declaration.binding])?);
                let binding = &declaration.binding;
                actor_locals.remove(&binding.name);
                if binding.actor_type_state.is_some()
                    || matches!(self.program.resolve(self.owner.module, &binding.source_type), Ok(ResolvedName::Declaration(id)) if id.kind == SymbolKind::ActorEnum)
                {
                    actor_locals.insert(binding.name.clone());
                }
                locals.insert(binding.name.clone());
            }
            EntryStatement::Plain { bindings, span, .. } => {
                self.bindings.unbound_names.extend(bindings.iter().map(|binding| binding.name.clone()));
                references.extend(self.span(body, *span, locals, &bindings.iter().collect::<Vec<_>>())?);
                for binding in bindings {
                    actor_locals.remove(&binding.name);
                    if binding.actor_type_state.is_some()
                        || matches!(self.program.resolve(self.owner.module, &binding.source_type), Ok(ResolvedName::Declaration(id)) if id.kind == SymbolKind::ActorEnum)
                    {
                        actor_locals.insert(binding.name.clone());
                    }
                }
                locals.extend(bindings.iter().map(|binding| binding.name.clone()));
            }
            EntryStatement::Become { routes, .. } | EntryStatement::ValidateOutputsBecome { routes, .. } => {
                for route in routes {
                    if let EntrySuccessor::Constructed { actor, state, .. } = &route.successor {
                        let target_source = body.span_text(*actor);
                        let target_tokens = lex(target_source)?;
                        let target = target_tokens
                            .iter()
                            .filter(|token| !matches!(token.kind, TokenKind::Eof))
                            .map(|token| &target_source[token.span.start..token.span.end])
                            .collect::<String>();
                        let target = target.as_str();
                        if !actor_locals.contains(target) {
                            if locals.contains(target) {
                                if self.entry_parameters.get(target) == Some(&true) {
                                    return Err(ArgentError::at(
                                        self.program.declaration_path(self.owner),
                                        format!("entry binding `{target}` collides with entry parameter of the same name",),
                                    ));
                                }
                                return Err(ArgentError::at(
                                    self.program.declaration_path(self.owner),
                                    format!("local `{target}` is not an actor selector"),
                                ));
                            }
                            match self.program.resolve(self.owner.module, target).map_err(|mut error| {
                                if !target.contains("::") {
                                    error.message.push_str(&format!("; actor handle `{target}` is not visible in this scope"));
                                }
                                error
                            })? {
                                ResolvedName::AppMember(_) => {}
                                ResolvedName::Declaration(id) if matches!(id.kind, SymbolKind::Actor | SymbolKind::ActorEnum) => {}
                                _ => {
                                    return Err(ArgentError::at(
                                        self.program.declaration_path(self.owner),
                                        format!("invalid successor actor target `{target}`"),
                                    ));
                                }
                            }
                        }
                        references.extend(self.span(body, *actor, locals, &[])?);
                        references.extend(self.span(body, *state, locals, &[])?);
                    }
                }
            }
        }
        Ok(())
    }

    fn span(
        &mut self,
        body: &EntryBody,
        span: Span,
        locals: &BTreeSet<String>,
        bindings: &[&EntryBinding],
    ) -> Result<Vec<BoundReference>> {
        let tokens = body.tokens();
        let start = tokens.partition_point(|token| token.span.start < span.start);
        let end = tokens.partition_point(|token| token.span.end <= span.end);
        self.references(body.text(), &tokens[start..end], locals, bindings)
    }

    fn references(
        &mut self,
        source: &str,
        tokens: &[Token],
        locals: &BTreeSet<String>,
        bindings: &[&EntryBinding],
    ) -> Result<Vec<BoundReference>> {
        let mut references = Vec::new();
        let mut cursor = 0;
        while cursor < tokens.len() {
            let start = cursor;
            cursor += 1;

            // keep units such as `seconds` in `1 seconds` out of declaration lookup
            if matches!(tokens[start].kind, TokenKind::Number(_)) {
                // lexer splits `1_000` into `1` and `_000`, and `1e3` into `1` and `e3`. group together.
                let mut end = cursor;
                while end < tokens.len()
                    && tokens[end - 1].span.end == tokens[end].span.start
                    && matches!(tokens[end].kind, TokenKind::Number(_) | TokenKind::Ident(_))
                {
                    end += 1;
                }

                // include the next word in case it is a unit
                if matches!(tokens.get(end).map(|token| &token.kind), Some(TokenKind::Ident(_))) {
                    end += 1;
                }
                let offset = tokens[start].span.start;

                // recognize the number literal at the start
                if let Ok(parsed) = parse_expression(&source[offset..tokens[end - 1].span.end])
                    && let Some(literal) = parsed.flatten().find(|pair| pair.as_rule() == Rule::number_literal)
                    && literal.as_span().start() == 0
                {
                    let literal_end = offset + literal.as_span().end();
                    // skip what has been accepted as number literal by Silver
                    while cursor < end && tokens[cursor].span.end <= literal_end {
                        cursor += 1;
                    }
                }
                continue;
            }

            let TokenKind::Ident(first) = &tokens[start].kind else {
                continue;
            };
            let mut segments = vec![first.as_str()];
            while matches!(tokens.get(cursor).map(|token| &token.kind), Some(TokenKind::Symbol(':')))
                && matches!(tokens.get(cursor + 1).map(|token| &token.kind), Some(TokenKind::Symbol(':')))
            {
                let Some(Token { kind: TokenKind::Ident(name), .. }) = tokens.get(cursor + 2) else {
                    break;
                };
                segments.push(name);
                cursor += 3;
            }
            let in_type = bindings.iter().any(|binding| {
                binding.type_span.is_some_and(|span| span.start <= tokens[start].span.start && tokens[start].span.end <= span.end)
            });
            if bindings.iter().any(|binding| binding.name_span == tokens[start].span)
                || (segments.len() == 1 && !in_type && locals.contains(first))
                || (segments.len() == 1
                    && start.checked_sub(1).is_some_and(|previous| matches!(tokens[previous].kind, TokenKind::Symbol('.'))))
                || (segments.len() == 1 && matches!(tokens.get(cursor).map(|token| &token.kind), Some(TokenKind::Symbol(':'))))
            {
                continue;
            }
            let mut count = segments.len();
            let mut bound = false;
            let actor_state =
                in_type && start.checked_sub(1).is_some_and(|previous| matches!(tokens[previous].kind, TokenKind::Symbol('<')));
            if actor_state
                || bindings.iter().any(|binding| binding.type_span.is_some_and(|span| span.start == tokens[start].span.start))
            {
                let name = segments.join("::");
                if actor_state {
                    self.required(&name, &[SymbolKind::State])?;
                } else if name != word::ACTOR_TYPE && !TypeRef::new(&name).is_builtin() {
                    self.required(&name, &[SymbolKind::State, SymbolKind::ActorEnum])?;
                }
            }
            while count > 0 {
                let name = segments[..count].join("::");
                if let Ok(target) = self.program.resolve(self.owner.module, &name) {
                    if matches!(target, ResolvedName::Module(_)) {
                        break;
                    }
                    if count < segments.len() {
                        let ResolvedName::Declaration(id) = target else { break };
                        let ResolvedDeclaration::ActorEnum(actor_enum) = self.program.declaration(id) else { break };
                        if count + 1 != segments.len() {
                            break;
                        }
                        let variant = segments[count];
                        let actor = actor_enum
                            .variants
                            .iter()
                            .find_map(|name| {
                                let ResolvedName::Declaration(actor) = self.program.resolve(id.module, name).ok()? else {
                                    return None;
                                };
                                (self.program.declaration(actor).name() == variant).then_some(actor)
                            })
                            .ok_or_else(|| {
                                ArgentError::at(
                                    self.program.declaration_path(self.owner),
                                    format!("actor enum `{}` has no variant `{variant}`", segments[..count].join("::")),
                                )
                            })?;
                        self.record(ResolvedName::Declaration(actor));
                        references
                            .push(BoundReference { span: tokens[start + count * 3].span, target: ResolvedName::Declaration(actor) });
                    }
                    self.record(target);
                    references.push(BoundReference {
                        span: Span { start: tokens[start].span.start, end: tokens[start + (count - 1) * 3].span.end },
                        target,
                    });
                    bound = true;
                    break;
                }
                count -= 1;
            }
            if !bound {
                if segments.len() > 1 {
                    return Err(ArgentError::at(
                        self.program.declaration_path(self.owner),
                        format!("unresolved qualified reference `{}`", segments.join("::")),
                    ));
                }
                self.bindings.unbound_names.insert(first.clone());
            }
        }
        Ok(references)
    }
}
