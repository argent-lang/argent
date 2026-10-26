//! Parses loaded source modules into Argent syntax declarations.
//!
//! Entry bodies and ordinary Sil syntax use the same token cursor.

use std::collections::BTreeMap;
use std::ops::Range;

use silverscript_lang::ast as sil;

use super::body::routes::analyze_authored_entry_routes;
use super::lexer::{Span, Token, TokenKind};
use super::node::{
    ChildEdge, DeclId, ModuleId, ModuleNodeIndex, NodeAddress, NodeId, ReferenceRole, RootSlot, SourceNodeCursor, SymbolKind,
};
use super::source::{Origin, SourceFile};
use super::word;
use super::*;
use crate::error::{ArgentError, Result};

#[derive(Debug, Clone)]
pub(crate) struct DiscoveredImport {
    pub(crate) import: Import,
    pub(crate) tokens: Range<usize>,
}

pub(crate) fn discover_imports(file: &SourceFile, tokens: &[Token]) -> Result<Vec<DiscoveredImport>> {
    let mut parser = Parser {
        file,
        source: &file.text,
        tokens,
        pos: 0,
        imports: &[],
        owner: None,
        member: None,
        nodes: ModuleNodeIndex::default(),
        const_values: Vec::new(),
        type_uses: BTreeMap::new(),
        function_bodies: BTreeMap::new(),
        entry_bodies: BTreeMap::new(),
        observe_exprs: BTreeMap::new(),
        name_paths: BTreeMap::new(),
        pending_type_uses: BTreeMap::new(),
        actor_targets: BTreeMap::new(),
        expression_end: None,
        entry_loop_depth: 0,
        next_route_id: 0,
    };
    let mut imports = Vec::new();
    while !parser.is_eof() {
        if parser.check_ident(word::IMPORT) {
            let start = parser.pos;
            let import = parser.parse_import()?;
            imports.push(DiscoveredImport { import, tokens: start..parser.pos });
            continue;
        }

        let block_terminated = [word::STATE, word::FN, word::ACTOR, word::APP].iter().any(|keyword| parser.check_ident(keyword));
        let (mut braces, mut parentheses, mut brackets) = (0usize, 0usize, 0usize);
        while !parser.is_eof() {
            let mut complete = false;
            match parser.current().kind {
                TokenKind::Symbol('{') => braces += 1,
                TokenKind::Symbol('}') => {
                    braces = braces.saturating_sub(1);
                    complete = block_terminated && braces == 0 && parentheses == 0 && brackets == 0;
                }
                TokenKind::Symbol('(') => parentheses += 1,
                TokenKind::Symbol(')') => parentheses = parentheses.saturating_sub(1),
                TokenKind::Symbol('[') => brackets += 1,
                TokenKind::Symbol(']') => brackets = brackets.saturating_sub(1),
                TokenKind::Symbol(';') => complete = !block_terminated && braces == 0 && parentheses == 0 && brackets == 0,
                _ => {}
            }
            parser.advance();
            if complete {
                break;
            }
        }
    }
    Ok(imports)
}

pub(crate) fn parse_module<'src>(
    file: &'src SourceFile,
    tokens: &'src [Token],
    imports: &'src [DiscoveredImport],
) -> Result<(SourceModule<'src>, ModuleNodeIndex)> {
    Parser {
        file,
        source: &file.text,
        tokens,
        pos: 0,
        imports,
        owner: None,
        member: None,
        nodes: ModuleNodeIndex::default(),
        const_values: Vec::new(),
        type_uses: BTreeMap::new(),
        function_bodies: BTreeMap::new(),
        entry_bodies: BTreeMap::new(),
        observe_exprs: BTreeMap::new(),
        name_paths: BTreeMap::new(),
        pending_type_uses: BTreeMap::new(),
        actor_targets: BTreeMap::new(),
        expression_end: None,
        entry_loop_depth: 0,
        next_route_id: 0,
    }
    .parse_module()
}

#[cfg(test)]
mod compatibility_tests;
mod entry;
mod expr;
mod index;
mod statements;
#[cfg(test)]
mod tests;
mod types;

struct Parser<'a> {
    file: &'a SourceFile,
    source: &'a str,
    tokens: &'a [Token],
    pos: usize,
    imports: &'a [DiscoveredImport],
    owner: Option<DeclId>,
    member: Option<RootSlot>,
    nodes: ModuleNodeIndex,
    const_values: Vec<sil::Expr<'a>>,
    type_uses: BTreeMap<NodeId, ArgentTypeUse>,
    function_bodies: BTreeMap<NodeId, Vec<sil::Statement<'a>>>,
    entry_bodies: BTreeMap<NodeId, Vec<AuthoredEntryStatement<'a>>>,
    observe_exprs: BTreeMap<NodeId, sil::Expr<'a>>,
    name_paths: BTreeMap<NodeId, NamePath>,
    pending_type_uses: BTreeMap<(usize, usize), ArgentTypeUse>,
    actor_targets: BTreeMap<NodeId, sil::Expr<'a>>,
    expression_end: Option<usize>,
    entry_loop_depth: usize,
    next_route_id: usize,
}

impl<'src> Parser<'src> {
    fn parse_module(mut self) -> Result<(SourceModule<'src>, ModuleNodeIndex)> {
        let mut module = Module {
            path: self.file.display_path.clone(),
            imports: Vec::new(),
            consts: Vec::new(),
            states: Vec::new(),
            functions: Vec::new(),
            actors: Vec::new(),
            actor_enums: Vec::new(),
            apps: Vec::new(),
        };

        while !self.is_eof() {
            let start = self.current().span.start;
            let owner = if self.check_ident(word::IMPORT) {
                if let Some(import) = self.imports.iter().find(|import| import.tokens.start == self.pos) {
                    module.imports.push(import.import.clone());
                    self.pos = import.tokens.end;
                } else {
                    return Err(self.error("import was not discovered before parsing"));
                }
                None
            } else if self.check_ident(word::CONST) {
                let id = DeclId::new(ModuleId::new(self.file.id.0), SymbolKind::Const, module.consts.len());
                self.owner = Some(id);
                module.consts.push(self.parse_const()?);
                Some(id)
            } else if self.check_ident(word::STATE) {
                let id = DeclId::new(ModuleId::new(self.file.id.0), SymbolKind::State, module.states.len());
                self.owner = Some(id);
                module.states.push(self.parse_state()?);
                Some(id)
            } else if self.check_ident(word::FN) {
                let id = DeclId::new(ModuleId::new(self.file.id.0), SymbolKind::Function, module.functions.len());
                self.owner = Some(id);
                module.functions.push(self.parse_function()?);
                Some(id)
            } else if self.check_ident(word::ACTOR) {
                if self.peek_ident(1, word::ENUM) {
                    let id = DeclId::new(ModuleId::new(self.file.id.0), SymbolKind::ActorEnum, module.actor_enums.len());
                    self.owner = Some(id);
                    module.actor_enums.push(self.parse_actor_enum()?);
                    Some(id)
                } else {
                    let id = DeclId::new(ModuleId::new(self.file.id.0), SymbolKind::Actor, module.actors.len());
                    self.owner = Some(id);
                    module.actors.push(self.parse_actor()?);
                    Some(id)
                }
            } else if self.check_ident(word::APP) {
                let id = DeclId::new(ModuleId::new(self.file.id.0), SymbolKind::App, module.apps.len());
                self.owner = Some(id);
                module.apps.push(self.parse_app()?);
                Some(id)
            } else {
                return Err(self.error(format!("expected top-level declaration, found {}", self.describe_current())));
            };
            if let Some(owner) = owner {
                self.nodes.insert(
                    NodeAddress { owner, root: RootSlot::Declaration, children: Vec::new() },
                    Origin::Authored { source: self.file.id, start, end: self.previous().span.end },
                );
                self.owner = None;
            }
        }

        Ok((
            SourceModule {
                source: self.file,
                legacy: module,
                const_values: self.const_values,
                type_uses: self.type_uses,
                function_bodies: self.function_bodies,
                entry_bodies: self.entry_bodies,
                observe_exprs: self.observe_exprs,
                name_paths: self.name_paths,
                actor_targets: self.actor_targets,
            },
            self.nodes,
        ))
    }

    fn parse_import(&mut self) -> Result<Import> {
        self.expect_ident(word::IMPORT)?;
        let path = self.expect_string()?;
        let alias = if self.consume_ident(word::AS) { Some(self.expect_any_ident()?) } else { None };
        let id = if self.consume_ident(word::ID) { Some(self.expect_string()?) } else { None };
        self.expect_symbol(';')?;
        Ok(Import { path, alias, id })
    }

    fn parse_const(&mut self) -> Result<ConstDecl> {
        self.expect_ident(word::CONST)?;
        let type_token_start = self.pos;
        let type_start = self.current().span.start;
        let (ty, type_use) = self.parse_type()?;
        let type_end = self.previous().span.end;
        let type_node = self.record_site(RootSlot::ConstType, Vec::new(), Span { start: type_start, end: type_end });
        self.type_uses.insert(type_node, type_use);
        if ty.array.is_some()
            && let Some(open) =
                self.tokens[type_token_start..self.pos].iter().position(|token| matches!(token.kind, TokenKind::Symbol('[')))
        {
            self.record_site(
                RootSlot::ConstType,
                vec![ChildEdge::TypeDimension(0)],
                Span { start: self.tokens[type_token_start + open].span.start, end: type_end },
            );
        }
        let name_span = self.current().span;
        let name = self.expect_any_ident()?;
        self.record_site(RootSlot::Name, Vec::new(), name_span);
        self.expect_symbol('=')?;
        let value_start = self.current().span.start;
        let value = self.parse_expression()?;
        let value_span = Span { start: value_start, end: self.previous().span.end.max(value_start) };
        self.expect_symbol(';')?;
        self.record_site(RootSlot::ConstValue, Vec::new(), value_span);
        let cursor = SourceNodeCursor::new(self.owner.expect("constant has a declaration"), RootSlot::ConstValue);
        self.index_expression(&cursor, &value, None);
        self.const_values.push(value);
        Ok(ConstDecl { ty, name })
    }

    fn parse_state(&mut self) -> Result<StateDecl> {
        self.expect_ident(word::STATE)?;
        let name_span = self.current().span;
        let name = self.expect_any_ident()?;
        self.record_site(RootSlot::Name, Vec::new(), name_span);
        let expands = if self.consume_ident(word::EXPANDS) {
            let start = self.current().span.start;
            let base = self.expect_any_qualified_ident()?;
            self.record_name_path(
                RootSlot::StateBase,
                Vec::new(),
                Span { start, end: self.previous().span.end },
                &base,
                ReferenceRole::Type,
            );
            Some(base)
        } else {
            None
        };
        self.expect_symbol('{')?;
        let mut fields = Vec::new();
        let mut digest_expansions = Vec::new();
        while !self.check_symbol('}') {
            if expands.is_some() {
                let field_span = self.current().span;
                let field = self.expect_any_ident()?;
                self.record_site(RootSlot::DigestField(digest_expansions.len()), Vec::new(), field_span);
                self.expect_symbol(':')?;
                let state_start = self.current().span.start;
                let state = self.expect_any_qualified_ident()?;
                self.record_name_path(
                    RootSlot::DigestState(digest_expansions.len()),
                    Vec::new(),
                    Span { start: state_start, end: self.previous().span.end },
                    &state,
                    ReferenceRole::Type,
                );
                self.expect_symbol(';')?;
                digest_expansions.push(StateDigestExpansionDecl { field, state });
            } else if self.consume_ident(word::VIRTUAL) {
                let field_span = self.current().span;
                let name = self.expect_any_ident()?;
                self.record_site(RootSlot::FieldName(fields.len()), Vec::new(), field_span);
                self.expect_symbol(';')?;
                fields.push(FieldDecl { ty: TypeRef::array("byte", 32), name, virtual_slot: true });
            } else {
                let type_start = self.current().span.start;
                let (ty, type_use) = self.parse_type()?;
                let type_node = self.record_site(
                    RootSlot::FieldType(fields.len()),
                    Vec::new(),
                    Span { start: type_start, end: self.previous().span.end },
                );
                self.type_uses.insert(type_node, type_use);
                let field_span = self.current().span;
                let name = self.expect_any_ident()?;
                self.record_site(RootSlot::FieldName(fields.len()), Vec::new(), field_span);
                self.expect_symbol(';')?;
                fields.push(FieldDecl { ty, name, virtual_slot: false });
            }
        }
        self.expect_symbol('}')?;
        let expansion = expands.map(|base| StateExpansionDecl { base, digests: digest_expansions });
        Ok(StateDecl { name, fields, expansion })
    }

    fn parse_function(&mut self) -> Result<FunctionDecl> {
        self.expect_ident(word::FN)?;
        let name_span = self.current().span;
        let name = self.expect_any_ident()?;
        if self.owner.is_some_and(|owner| owner.kind() == SymbolKind::Function) {
            self.record_site(RootSlot::Name, Vec::new(), name_span);
        }
        let params = self.parse_param_list()?;
        let return_ty = if self.consume_arrow() {
            let start = self.current().span.start;
            let (ty, type_use) = self.parse_type()?;
            let root = self.member.unwrap_or(RootSlot::Declaration);
            let type_node = self.record_site(root, vec![ChildEdge::ReturnType], Span { start, end: self.previous().span.end });
            self.type_uses.insert(type_node, type_use);
            Some(ty)
        } else {
            None
        };
        let body_start = self.current().span.start;
        self.expect_symbol('{')?;
        let statements = self.parse_sil_statement_sequence()?;
        self.expect_symbol('}')?;
        let root = self.member.unwrap_or(RootSlot::Declaration);
        let body_node = self.record_site(root, vec![ChildEdge::Body], Span { start: body_start, end: self.previous().span.end });
        let cursor = SourceNodeCursor::new(self.owner.expect("function has a declaration"), root).child(ChildEdge::Body);
        self.index_statement_sequence(&cursor, &statements);
        self.function_bodies.insert(body_node, statements);
        Ok(FunctionDecl { name, params, return_ty })
    }

    fn parse_actor(&mut self) -> Result<ActorDecl> {
        self.expect_ident(word::ACTOR)?;
        let name_span = self.current().span;
        let name = self.expect_any_ident()?;
        self.record_site(RootSlot::Name, Vec::new(), name_span);
        self.expect_ident(word::OWNS)?;
        let state_start = self.current().span.start;
        let state = self.expect_any_qualified_ident()?;
        self.record_name_path(
            RootSlot::ActorState,
            Vec::new(),
            Span { start: state_start, end: self.previous().span.end },
            &state,
            ReferenceRole::Type,
        );
        self.expect_symbol('{')?;
        let mut functions = Vec::new();
        let mut entries = Vec::new();
        while !self.check_symbol('}') {
            if self.check_ident(word::FN) {
                let start = self.current().span.start;
                let name_span = self.tokens.get(self.pos + 1).map_or(self.current().span, |token| token.span);
                self.member = Some(RootSlot::ActorFunction(functions.len()));
                functions.push(self.parse_function()?);
                let root = RootSlot::ActorFunction(functions.len() - 1);
                self.record_site(root, Vec::new(), Span { start, end: self.previous().span.end });
                self.record_site(root, vec![ChildEdge::Name], name_span);
                self.member = None;
            } else {
                let start = self.current().span.start;
                let name_span = self.tokens.get(self.pos + 1).map_or(self.current().span, |token| token.span);
                self.member = Some(RootSlot::Entry(entries.len()));
                entries.push(self.parse_actor_item()?);
                let root = RootSlot::Entry(entries.len() - 1);
                self.record_site(root, Vec::new(), Span { start, end: self.previous().span.end });
                self.record_site(root, vec![ChildEdge::Name], name_span);
                self.member = None;
            }
        }
        self.expect_symbol('}')?;
        Ok(ActorDecl { name, state, functions, entries })
    }

    fn parse_actor_enum(&mut self) -> Result<ActorEnumDecl> {
        self.expect_ident(word::ACTOR)?;
        self.expect_ident(word::ENUM)?;
        let name_span = self.current().span;
        let name = self.expect_any_ident()?;
        self.record_site(RootSlot::Name, Vec::new(), name_span);
        self.expect_symbol('{')?;
        let mut variants = Vec::new();
        while !self.check_symbol('}') {
            let start = self.current().span.start;
            variants.push(self.expect_any_qualified_ident()?);
            self.record_site(
                RootSlot::ActorEnumVariant(variants.len() - 1),
                Vec::new(),
                Span { start, end: self.previous().span.end },
            );
            if self.consume_symbol(';') || self.consume_symbol(',') {
                continue;
            }
            if !self.check_symbol('}') {
                return Err(self.error(format!("expected `;`, `,`, or `}}`, found {}", self.describe_current())));
            }
        }
        self.expect_symbol('}')?;
        Ok(ActorEnumDecl { name, variants })
    }

    fn parse_actor_item(&mut self) -> Result<EntryDecl> {
        if self.check_ident(word::ENTRY) {
            self.parse_entry()
        } else if self.check_ident(word::DELEGATE) {
            self.parse_delegate()
        } else {
            Err(self.error(format!("expected `fn`, `entry`, or `delegate`, found {}", self.describe_current())))
        }
    }

    fn parse_entry(&mut self) -> Result<EntryDecl> {
        self.expect_ident(word::ENTRY)?;
        let name = self.expect_any_ident()?;
        let params = self.parse_param_list()?;
        let (observes, consumes, spawns) = self.parse_entry_clauses()?;
        self.expect_ident(word::EMITS)?;
        let emits = self.parse_emits()?;
        let body_start = self.current().span.start;
        self.next_route_id = 0;
        self.expect_symbol('{')?;
        let statements = self.parse_entry_statement_sequence()?;
        let content_end = self.current().span.start;
        self.expect_symbol('}')?;
        let body_node = self.record_site(
            self.member.expect("entry belongs to an actor"),
            vec![ChildEdge::Body],
            Span { start: body_start, end: self.previous().span.end },
        );
        let cursor =
            SourceNodeCursor::new(self.owner.expect("entry has a declaration"), self.member.expect("entry belongs to an actor"))
                .child(ChildEdge::Body);
        self.index_entry_sequence(&cursor, &statements);
        let route_analysis = analyze_authored_entry_routes(&statements, self.file, content_end)?;
        self.entry_bodies.insert(body_node, statements);
        Ok(EntryDecl {
            kind: EntryKind::Leader,
            name,
            params,
            consumes,
            observes,
            spawns,
            emits,
            routes: route_analysis.routes,
            terminal_route_sets: route_analysis.terminal_route_sets,
        })
    }

    fn parse_delegate(&mut self) -> Result<EntryDecl> {
        self.expect_ident(word::DELEGATE)?;
        let name = self.expect_any_ident()?;
        let params = self.parse_param_list()?;
        let (observes, consumes, spawns) = self.parse_entry_clauses()?;
        let body_start = self.current().span.start;
        self.next_route_id = 0;
        self.expect_symbol('{')?;
        let statements = self.parse_entry_statement_sequence()?;
        let content_end = self.current().span.start;
        self.expect_symbol('}')?;
        let body_node = self.record_site(
            self.member.expect("delegate belongs to an actor"),
            vec![ChildEdge::Body],
            Span { start: body_start, end: self.previous().span.end },
        );
        let cursor =
            SourceNodeCursor::new(self.owner.expect("delegate has a declaration"), self.member.expect("delegate belongs to an actor"))
                .child(ChildEdge::Body);
        self.index_entry_sequence(&cursor, &statements);
        let route_analysis = analyze_authored_entry_routes(&statements, self.file, content_end)?;
        self.entry_bodies.insert(body_node, statements);
        Ok(EntryDecl {
            kind: EntryKind::Delegate,
            name,
            params,
            consumes,
            observes,
            spawns,
            emits: EmitSpec::None,
            routes: route_analysis.routes,
            terminal_route_sets: route_analysis.terminal_route_sets,
        })
    }

    fn parse_app(&mut self) -> Result<AppDecl> {
        self.expect_ident(word::APP)?;
        let name_span = self.current().span;
        let name = self.expect_any_ident()?;
        self.record_site(RootSlot::Name, Vec::new(), name_span);
        self.expect_symbol('{')?;
        let mut actors = Vec::new();
        while !self.check_symbol('}') {
            if self.consume_ident(word::ACTOR) {
                let start = self.current().span.start;
                actors.push(self.expect_any_qualified_ident()?);
                self.record_name_path(
                    RootSlot::AppActor(actors.len() - 1),
                    Vec::new(),
                    Span { start, end: self.previous().span.end },
                    actors.last().expect("actor was just parsed"),
                    ReferenceRole::ActorTarget,
                );
                self.expect_symbol(';')?;
            } else {
                return Err(self.error(format!("expected `actor`, found {}", self.describe_current())));
            }
        }
        self.expect_symbol('}')?;
        Ok(AppDecl { name, actors })
    }

    fn parse_param_list(&mut self) -> Result<Vec<ParamDecl>> {
        self.expect_symbol('(')?;
        let mut params = Vec::new();
        while !self.check_symbol(')') {
            let type_start = self.current().span.start;
            let (ty, type_use) = self.parse_type()?;
            let root = self.member.unwrap_or(RootSlot::Declaration);
            let type_node = self.record_site(
                root,
                vec![ChildEdge::ParamType(params.len())],
                Span { start: type_start, end: self.previous().span.end },
            );
            self.type_uses.insert(type_node, type_use);
            let name_span = self.current().span;
            let name = self.expect_any_ident()?;
            self.record_site(root, vec![ChildEdge::ParamName(params.len())], name_span);
            params.push(ParamDecl { name, ty });
            if self.consume_symbol(',') {
                continue;
            }
            break;
        }
        self.expect_symbol(')')?;
        Ok(params)
    }

    fn parse_consumes(&mut self) -> Result<Vec<ConsumeDecl>> {
        self.expect_ident(word::CONSUMES)?;
        self.expect_symbol('{')?;
        let mut consumes = Vec::new();
        while !self.check_symbol('}') {
            let name = self.expect_any_ident()?;
            self.expect_symbol(':')?;
            let actor_start = self.current().span.start;
            let actor = self.expect_any_qualified_ident()?;
            self.record_name_path(
                self.member.expect("consumes belongs to an entry"),
                vec![ChildEdge::Consume(consumes.len()), ChildEdge::ActorTarget],
                Span { start: actor_start, end: self.previous().span.end },
                &actor,
                ReferenceRole::ActorTarget,
            );
            let cursor = SourceNodeCursor::new(self.owner.expect("entry has a declaration"), self.member.expect("entry clause"))
                .child(ChildEdge::Consume(consumes.len()));
            let cardinality = self.parse_cardinality(&cursor)?;
            consumes.push(ConsumeDecl { name, actor, cardinality });
            self.expect_list_separator_or_end('}')?;
        }
        self.expect_symbol('}')?;
        Ok(consumes)
    }

    fn parse_entry_clauses(&mut self) -> Result<(Vec<ObserveDecl>, Vec<ConsumeDecl>, Vec<SpawnDecl>)> {
        let mut observes = Vec::new();
        let mut consumes = Vec::new();
        let mut spawns = Vec::new();
        let mut parsed_consumes = false;
        loop {
            if self.check_ident(word::OBSERVES) {
                observes.push(self.parse_observes(observes.len())?);
            } else if self.check_ident(word::SPAWNS) {
                spawns.push(self.parse_spawns(spawns.len())?);
            } else if self.check_ident(word::CONSUMES) {
                if parsed_consumes {
                    return Err(self.error("entry declares `consumes` more than once"));
                }
                consumes = self.parse_consumes()?;
                parsed_consumes = true;
            } else {
                break;
            }
        }
        Ok((observes, consumes, spawns))
    }

    fn parse_spawns(&mut self, index: usize) -> Result<SpawnDecl> {
        self.expect_ident(word::SPAWNS)?;
        let name = self.expect_any_ident()?;
        self.expect_ident(word::BY)?;
        let covenant_start = self.current().span.start;
        let covenant = self.expect_any_ident()?;
        self.record_name_path(
            self.member.expect("spawn belongs to an entry"),
            vec![ChildEdge::Spawn(index), ChildEdge::Covenant],
            Span { start: covenant_start, end: self.previous().span.end },
            &covenant,
            ReferenceRole::ClauseTarget,
        );
        self.expect_symbol('{')?;
        self.expect_ident(word::OUTPUTS)?;
        self.expect_symbol('{')?;
        let mut outputs = Vec::new();
        while !self.check_symbol('}') {
            let name = self.expect_any_ident()?;
            self.expect_symbol(':')?;
            let site = SourceNodeCursor::new(self.owner.expect("entry has a declaration"), self.member.expect("entry clause"))
                .child(ChildEdge::Spawn(index))
                .child(ChildEdge::SpawnOutput(outputs.len()));
            let (actor, cardinality, actor_expr) = self.take_clause_actor_target(&site)?;
            let root = self.member.expect("spawn belongs to an entry");
            let children = vec![ChildEdge::Spawn(index), ChildEdge::SpawnOutput(outputs.len()), ChildEdge::ActorTarget];
            let node = self.record_site(root, children.clone(), Span { start: actor_expr.span.start(), end: actor_expr.span.end() });
            let cursor = SourceNodeCursor::new(self.owner.expect("entry has a declaration"), root);
            let cursor = children.into_iter().fold(cursor, |cursor, edge| cursor.child(edge));
            self.index_expression(&cursor, &actor_expr, Some(ReferenceRole::ActorTarget));
            self.actor_targets.insert(node, actor_expr);
            outputs.push(SpawnOutputDecl { name, actor, cardinality, group_index: outputs.len() });
            self.expect_list_separator_or_end('}')?;
        }
        self.expect_symbol('}')?;
        self.expect_symbol('}')?;
        Ok(SpawnDecl { name, covenant, outputs })
    }

    fn parse_observes(&mut self, index: usize) -> Result<ObserveDecl> {
        self.expect_ident(word::OBSERVES)?;
        let name = self.expect_any_ident()?;
        self.expect_ident(word::BY)?;
        let covenant_expr_start = self.current().span.start;
        let block_start = (self.pos..self.tokens.len())
            .find(|&position| matches!(self.tokens[position].kind, TokenKind::Symbol('{') | TokenKind::Eof))
            .ok_or_else(|| self.error("observes clause has no body"))?;
        self.expression_end = Some(block_start);
        let covenant_ast = self.parse_expression();
        self.expression_end = None;
        let covenant_ast = covenant_ast?;
        if self.pos != block_start {
            return Err(self.error("invalid observes covenant expression"));
        }
        let covenant_expr = self.source[covenant_expr_start..self.current().span.start].trim().to_string();
        if covenant_expr.is_empty() {
            return Err(self.error("observes clause has an empty covenant expression"));
        }
        let root = self.member.expect("observes clause belongs to an entry");
        let node = self.record_site(
            root,
            vec![ChildEdge::ObserveCovenant(index)],
            Span { start: covenant_expr_start, end: self.current().span.start },
        );
        let cursor =
            SourceNodeCursor::new(self.owner.expect("entry has a declaration"), root).child(ChildEdge::ObserveCovenant(index));
        self.index_expression(&cursor, &covenant_ast, None);
        self.observe_exprs.insert(node, covenant_ast);

        self.expect_symbol('{')?;
        let mut inputs = None;
        let mut outputs = None;
        while !self.check_symbol('}') {
            if self.check_ident(word::INPUTS) {
                if inputs.is_some() {
                    return Err(self.error("observes clause declares `inputs` more than once"));
                }
                inputs = Some(self.parse_observed_actor_list(index, word::INPUTS)?);
            } else if self.check_ident(word::OUTPUTS) {
                if outputs.is_some() {
                    return Err(self.error("observes clause declares `outputs` more than once"));
                }
                outputs = Some(self.parse_observed_actor_list(index, word::OUTPUTS)?);
            } else {
                return Err(self.error(format!("expected `inputs` or `outputs`, found {}", self.describe_current())));
            }
        }
        self.expect_symbol('}')?;

        Ok(ObserveDecl { name, covenant_expr, inputs: inputs.unwrap_or_default(), outputs: outputs.unwrap_or_default() })
    }

    fn parse_observed_actor_list(&mut self, observe_index: usize, section: &str) -> Result<Vec<ObservedActorDecl>> {
        self.expect_ident(section)?;
        self.expect_symbol('{')?;
        let mut actors = Vec::new();
        while !self.check_symbol('}') {
            let name = self.expect_any_ident()?;
            self.expect_symbol(':')?;
            let root = self.member.expect("observes belongs to an entry");
            let edge =
                if section == word::INPUTS { ChildEdge::ObservedInput(actors.len()) } else { ChildEdge::ObservedOutput(actors.len()) };
            let children = vec![ChildEdge::Observe(observe_index), edge];
            let (actor, open_state, cardinality) = if self.consume_ident(word::ACTOR_TYPE) {
                if section != word::INPUTS {
                    return Err(self.error("open observed actor bindings are only declared in `inputs`"));
                }
                self.expect_symbol('<')?;
                let state_start = self.current().span.start;
                let state = self.expect_any_qualified_ident()?;
                self.record_name_path(
                    root,
                    [children.clone(), vec![ChildEdge::OpenState]].concat(),
                    Span { start: state_start, end: self.previous().span.end },
                    &state,
                    ReferenceRole::Type,
                );
                self.expect_symbol('>')?;
                self.expect_ident(word::AS)?;
                let actor_start = self.current().span.start;
                let actor = self.expect_any_ident()?;
                self.record_name_path(
                    root,
                    [children.clone(), vec![ChildEdge::ActorTarget]].concat(),
                    Span { start: actor_start, end: self.previous().span.end },
                    &actor,
                    ReferenceRole::ActorTarget,
                );
                let site = SourceNodeCursor::new(self.owner.expect("entry has a declaration"), root);
                let site = children.iter().copied().fold(site, |cursor, edge| cursor.child(edge));
                let cardinality = self.parse_cardinality(&site)?;
                (actor, Some(state), cardinality)
            } else {
                let site = SourceNodeCursor::new(self.owner.expect("entry has a declaration"), root);
                let site = children.iter().copied().fold(site, |cursor, edge| cursor.child(edge));
                let (actor, cardinality, actor_expr) = self.take_clause_actor_target(&site)?;
                let children = [children, vec![ChildEdge::ActorTarget]].concat();
                let node =
                    self.record_site(root, children.clone(), Span { start: actor_expr.span.start(), end: actor_expr.span.end() });
                let cursor = SourceNodeCursor::new(self.owner.expect("entry has a declaration"), root);
                let cursor = children.into_iter().fold(cursor, |cursor, edge| cursor.child(edge));
                self.index_expression(&cursor, &actor_expr, Some(ReferenceRole::ActorTarget));
                self.actor_targets.insert(node, actor_expr);
                (actor, None, cardinality)
            };
            actors.push(ObservedActorDecl { name, actor, open_state, cardinality });
            self.expect_list_separator_or_end('}')?;
        }
        self.expect_symbol('}')?;
        Ok(actors)
    }

    fn take_clause_actor_target(&mut self, site: &SourceNodeCursor) -> Result<(String, Cardinality, sil::Expr<'src>)> {
        let token_start = self.pos;
        let start = self.current().span.start;
        let mut depth = 0usize;
        while !self.is_eof() {
            let token = self.current().clone();
            match token.kind {
                TokenKind::Symbol('{') | TokenKind::Symbol('(') | TokenKind::Symbol('[') | TokenKind::Symbol('<') => {
                    depth += 1;
                    self.advance();
                }
                TokenKind::Symbol(',' | '}' | ';') if depth == 0 => {
                    let token_end = self.pos;
                    let cardinality_start = self.cardinality_suffix_start(token_start, token_end);
                    let actor_end = cardinality_start.map(|pos| self.tokens[pos].span.start).unwrap_or(token.span.start);
                    let actor = self.source[start..actor_end].trim().to_string();
                    if actor.is_empty() {
                        return Err(self.error("actor target is empty"));
                    }
                    let cardinality = if let Some(cardinality_start) = cardinality_start {
                        self.pos = cardinality_start;
                        let cardinality = self.parse_cardinality(site)?;
                        debug_assert_eq!(self.pos, token_end);
                        cardinality
                    } else {
                        Cardinality::One
                    };
                    self.pos = token_start;
                    self.expression_end = Some(cardinality_start.unwrap_or(token_end));
                    let actor_expr = self.parse_expression();
                    self.expression_end = None;
                    let actor_expr = actor_expr?;
                    if self.pos != cardinality_start.unwrap_or(token_end) {
                        return Err(self.error("invalid actor target expression"));
                    }
                    self.pos = token_end;
                    return Ok((actor, cardinality, actor_expr));
                }
                TokenKind::Symbol('}') | TokenKind::Symbol(')') | TokenKind::Symbol(']') | TokenKind::Symbol('>') => {
                    depth = depth.saturating_sub(1);
                    self.advance();
                }
                _ => self.advance(),
            }
        }
        Err(self.error("unterminated actor target"))
    }

    /// Identify a trailing `[minimum..=maximum]` without confusing brackets
    /// that are part of the actor target expression.
    fn cardinality_suffix_start(&self, token_start: usize, token_end: usize) -> Option<usize> {
        let close = token_end.checked_sub(1)?;
        if !matches!(self.tokens.get(close)?.kind, TokenKind::Symbol(']')) {
            return None;
        }

        let mut depth = 0usize;
        let mut open = None;
        for pos in (token_start..=close).rev() {
            match self.tokens[pos].kind {
                TokenKind::Symbol(']') => depth += 1,
                TokenKind::Symbol('[') => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        open = Some(pos);
                        break;
                    }
                }
                _ => {}
            }
        }
        let open = open?;
        self.has_range_marker(open + 1, close).then_some(open)
    }

    fn has_range_marker(&self, start: usize, end: usize) -> bool {
        (start..end.saturating_sub(1)).any(|pos| {
            matches!(self.tokens[pos].kind, TokenKind::Symbol('.')) && matches!(self.tokens[pos + 1].kind, TokenKind::Symbol('.'))
        })
    }

    fn parse_emits(&mut self) -> Result<EmitSpec> {
        if self.consume_ident(word::NONE) {
            Ok(EmitSpec::None)
        } else if self.check_symbol('{') {
            self.expect_symbol('{')?;
            let mut outputs = Vec::new();
            while !self.check_symbol('}') {
                let name = self.expect_any_ident()?;
                self.expect_symbol(':')?;
                let actors = self.parse_actor_union(outputs.len())?;
                let site = SourceNodeCursor::new(self.owner.expect("entry has a declaration"), self.member.expect("entry clause"))
                    .child(ChildEdge::EmitOutput(outputs.len()));
                let cardinality = self.parse_cardinality(&site)?;
                let auth_index = outputs.len();
                outputs.push(EmitOutput { name, actors, cardinality, auth_index });
                self.expect_list_separator_or_end('}')?;
            }
            self.expect_symbol('}')?;
            Ok(EmitSpec::Outputs(outputs))
        } else {
            let name = self.expect_any_ident()?;
            if name == word::ONE && !self.check_symbol(':') {
                return Err(self.error("`emits one Type` has been removed; declare a named output with `emits name: Type`"));
            }
            self.expect_symbol(':')?;
            let actors = self.parse_actor_union(0)?;
            let site = SourceNodeCursor::new(self.owner.expect("entry has a declaration"), self.member.expect("entry clause"))
                .child(ChildEdge::EmitOutput(0));
            let cardinality = self.parse_cardinality(&site)?;
            Ok(EmitSpec::Outputs(vec![EmitOutput { name, actors, cardinality, auth_index: 0 }]))
        }
    }

    fn parse_actor_union(&mut self, output_index: usize) -> Result<Vec<String>> {
        let mut actors = Vec::new();
        loop {
            let start = self.current().span.start;
            actors.push(self.expect_any_qualified_ident()?);
            self.record_name_path(
                self.member.expect("emits belongs to an entry"),
                vec![ChildEdge::EmitOutput(output_index), ChildEdge::Element(actors.len() - 1)],
                Span { start, end: self.previous().span.end },
                actors.last().expect("actor was just parsed"),
                ReferenceRole::ActorTarget,
            );
            if !self.consume_symbol('|') {
                break;
            }
        }
        Ok(actors)
    }

    fn parse_cardinality(&mut self, site: &SourceNodeCursor) -> Result<Cardinality> {
        if !self.consume_symbol('[') {
            return Ok(Cardinality::One);
        }
        let minimum = self.parse_cardinality_bound(&site.child(ChildEdge::CardinalityMin))?;
        self.expect_symbol('.')?;
        self.expect_symbol('.')?;
        self.expect_symbol('=')?;
        let maximum = self.parse_cardinality_bound(&site.child(ChildEdge::CardinalityMax))?;
        self.expect_symbol(']')?;
        Ok(Cardinality::Range { minimum, maximum })
    }

    fn parse_cardinality_bound(&mut self, site: &SourceNodeCursor) -> Result<CardinalityBound> {
        let negative = self.consume_symbol('-');
        match self.current().kind.clone() {
            TokenKind::Number(value) => {
                self.advance();
                let value = super::lexer::parse_number_value(&value).ok_or_else(|| self.error("range bound integer is too large"))?;
                let value =
                    if negative { value.checked_neg().ok_or_else(|| self.error("range bound integer is too small"))? } else { value };
                Ok(CardinalityBound::Literal(value))
            }
            TokenKind::Ident(_) if !negative => {
                let start = self.current().span.start;
                let value = self.expect_any_qualified_ident()?;
                self.record_name_path(
                    site.address.root,
                    site.address.children.clone(),
                    Span { start, end: self.previous().span.end },
                    &value,
                    ReferenceRole::Value,
                );
                Ok(CardinalityBound::Const(value))
            }
            _ => Err(self.error("range bound must be an integer literal or const identifier")),
        }
    }

    fn expect_ident(&mut self, expected: &str) -> Result<()> {
        match &self.current().kind {
            TokenKind::Ident(actual) if actual == expected => {
                self.advance();
                Ok(())
            }
            _ => Err(self.error(format!("expected `{expected}`, found {}", self.describe_current()))),
        }
    }

    fn consume_ident(&mut self, expected: &str) -> bool {
        match &self.current().kind {
            TokenKind::Ident(actual) if actual == expected => {
                self.advance();
                true
            }
            _ => false,
        }
    }

    fn peek_ident(&self, offset: usize, expected: &str) -> bool {
        matches!(self.tokens.get(self.pos + offset).map(|token| &token.kind), Some(TokenKind::Ident(actual)) if actual == expected)
    }

    fn expect_any_ident(&mut self) -> Result<String> {
        match self.current().kind.clone() {
            TokenKind::Ident(name) => {
                self.advance();
                Ok(name)
            }
            _ => Err(self.error(format!("expected identifier, found {}", self.describe_current()))),
        }
    }

    /// Same as [expect_any_ident], with optional namespace qualification.
    fn expect_any_qualified_ident(&mut self) -> Result<String> {
        let mut name = self.expect_any_ident()?;
        while self.consume_symbol(':') {
            self.expect_symbol(':')?;
            name.push_str("::");
            name.push_str(&self.expect_any_ident()?);
        }
        Ok(name)
    }

    fn check_ident(&self, expected: &str) -> bool {
        matches!(&self.current().kind, TokenKind::Ident(actual) if actual == expected)
    }

    fn expect_string(&mut self) -> Result<String> {
        match self.current().kind.clone() {
            TokenKind::Str(value) => {
                self.advance();
                Ok(value)
            }
            _ => Err(self.error(format!("expected string, found {}", self.describe_current()))),
        }
    }

    fn expect_symbol(&mut self, expected: char) -> Result<()> {
        match self.current().kind {
            TokenKind::Symbol(actual) if actual == expected => {
                self.advance();
                Ok(())
            }
            _ => Err(self.error(format!("expected `{expected}`, found {}", self.describe_current()))),
        }
    }

    fn consume_symbol(&mut self, expected: char) -> bool {
        match self.current().kind {
            TokenKind::Symbol(actual) if actual == expected => {
                self.advance();
                true
            }
            _ => false,
        }
    }

    fn expect_list_separator_or_end(&mut self, end: char) -> Result<()> {
        if self.consume_symbol(',') || self.check_symbol(end) {
            Ok(())
        } else {
            Err(self.error(format!("expected `,` or `{end}`, found {}", self.describe_current())))
        }
    }

    fn check_symbol(&self, expected: char) -> bool {
        matches!(self.current().kind, TokenKind::Symbol(actual) if actual == expected)
    }

    fn consume_arrow(&mut self) -> bool {
        if matches!(self.current().kind, TokenKind::Arrow) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn current(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn previous(&self) -> &Token {
        &self.tokens[self.pos - 1]
    }

    fn advance(&mut self) {
        if !self.is_eof() {
            self.pos += 1;
        }
    }

    fn is_eof(&self) -> bool {
        matches!(self.current().kind, TokenKind::Eof)
    }

    fn describe_current(&self) -> String {
        match &self.current().kind {
            TokenKind::Ident(value) => format!("identifier `{value}`"),
            TokenKind::Number(value) => format!("number `{value}`"),
            TokenKind::Str(value) => format!("string \"{value}\""),
            TokenKind::Arrow => "`->`".to_string(),
            TokenKind::Symbol(value) => format!("`{value}`"),
            TokenKind::Eof => "end of file".to_string(),
        }
    }

    fn error(&self, message: impl Into<String>) -> ArgentError {
        self.file.error_at(self.current().span.start, message)
    }

    fn record_site(&mut self, root: RootSlot, children: Vec<ChildEdge>, span: Span) -> NodeId {
        let owner = self.owner.expect("authored site belongs to a declaration");
        self.nodes
            .insert(NodeAddress { owner, root, children }, Origin::Authored { source: self.file.id, start: span.start, end: span.end })
    }

    fn record_name_path(&mut self, root: RootSlot, children: Vec<ChildEdge>, span: Span, name: &str, role: ReferenceRole) -> NodeId {
        let owner = self.owner.expect("authored name path belongs to a declaration");
        let origin = Origin::Authored { source: self.file.id, start: span.start, end: span.end };
        let node = self.nodes.insert_with_role(NodeAddress { owner, root, children }, origin, Some(role));
        self.name_paths.insert(node, NamePath { segments: name.split("::").map(str::to_string).collect(), origin });
        node
    }
}
