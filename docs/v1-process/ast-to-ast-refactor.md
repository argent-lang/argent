# AST-to-AST compiler refactor

This document sketches the target AST architecture.

## End goal

```text
.ag source
    |
  parse()
    v
Program                         Argent syntax with nested Sil AST nodes
    |
  resolve_and_plan()
    v
ResolvedProgram                 bindings, types, routes, layouts, witnesses
    |
    +-- lower_to_sil() --> ContractAst per actor -- format_contract_ast() --> .sil source
    |                                |
    |                                +-- compile_contract_ast() --> compiled Sil contracts
    |                                                                  |
    +------------------------------------------------------------------+
                                                                       |
                                                                build_artifact()
                                                                       |
                                                                       v
                                                                Argent artifact
```

The generated `.sil` and the bytecode must come from the same final
Silverscript `ContractAst`.

## Combined source AST

Argent owns application syntax. Ordinary code is represented by the existing
Silverscript AST.

```rust
use silverscript_lang::ast as sil;

pub struct Program {
    modules: Vec<Module>,
}

pub struct Module {
    imports: Vec<Import>,
    constants: Vec<ConstDecl>,
    states: Vec<StateDecl>,
    functions: Vec<FunctionDecl>, // global fns
    actors: Vec<ActorDecl>,
    actor_enums: Vec<ActorEnumDecl>,
    apps: Vec<AppDecl>,
}

pub struct FunctionDecl {
    name: Name,
    params: Vec<ParamDecl>,
    return_type: Option<sil::TypeRef>,
    body: Vec<sil::Statement>,
}

pub struct ActorDecl {
    name: Name,
    state: Name,
    functions: Vec<FunctionDecl>, // actor-level fns
    entries: Vec<EntryDecl>,
}

pub struct EntryDecl {
    kind: EntryKind,
    name: Name,
    params: Vec<ParamDecl>,
    consumes: Vec<ConsumeDecl>,
    observes: Vec<ObserveDecl>,
    spawns: Vec<SpawnDecl>,
    emits: EmitDecl, // currently EmitSpec
    body: Block,
}
```

Types and expressions inside these nodes use `sil::TypeRef` and `sil::Expr`.
Helper bodies use `sil::Statement` directly.

Entry bodies need a small Argent envelope because `become` can occur inside an
`if` or lexical block.

```rust
pub struct Block {
    statements: Vec<EntryStatement>,
}

pub enum EntryStatement {
    Sil(sil::Statement),
    
    Block(Block),
    
    If {
        condition: sil::Expr,
        then_body: Block,
        else_body: Option<Block>,
    },
    
    // Currently we also have `For { ... }`. Remove it when ordinary nested Sil is stored as
    // `Sil(sil::Statement::For { ... })` since `become` is not allowed in loops anyway.
    // For {
    //     ...
    // },

    Become(Vec<Route>),
    
    // Currently `ValidateOutputsBecome`.
    ObservedBecome {
        group: Name,
        routes: Vec<Route>,
    },
}

pub struct Route {
    output: Name,
    successor: Successor,
}

pub enum Successor {
    ExactSelf,
    Constructed {
        target: ActorTarget,
        state: sil::Expr,
        arity: RouteArity,
    },
}
```

`Sil` can contain ordinary Silverscript control flow, including `for`. Argent
rejects `become` inside a loop, so the final combined AST needs no Argent
`For` variant.

Every authored node retains its source file and span. Generated nodes use
synthetic spans. Source text is kept for diagnostics, not as a semantic node.

The Silverscript AST API is fixed. Argent owns the combined envelope and any
adapters that it needs.

### Co-spent call syntax

Argent must replace its one expression-level method call:

```rust
id.co_spent()       // current
co_spent(id)        // target
```

Silverscript has no generic method-call AST. The normal call fits
`sil::ExprKind::Call`, works inside global and actor functions, and removes the
current token-based `.co_spent()` lowering.

## Resolved program

The parsed AST records syntax. The resolved program records meaning.

```rust
pub struct ResolvedProgram {
    syntax: Program,
    symbols: SymbolTable,
    types: TypeTable,
    app: ResolvedApp,
    actors: BTreeMap<ActorId, ResolvedActor>,
    routing: RoutePlan,
}

pub struct ResolvedActor {
    syntax: NodeId,
    state: SourceStateId,
    layout: ContractStateLowering,
    entries: BTreeMap<EntryId, ResolvedEntry>,
}

pub struct ResolvedEntry {
    syntax: NodeId,
    bindings: BindingTable,
    interactions: Vec<InteractionPlan>,
    locations: InteractionLocationPlan,
    inputs: InputReferencePlan,
    routes: BTreeMap<RouteId, ResolvedRoute>,
    witnesses: WitnessPlan,
}
```

Argent-visible names resolve to stable IDs. State, route, template, input, and
generated-field provenance remains typed in this model.

## Lowering

```rust
pub fn lower_to_sil(
    program: &ResolvedProgram,
) -> Result<BTreeMap<ActorId, sil::ContractAst>>;
```

The lowerer removes all Argent operations:

```text
input references      -> authenticated Sil expressions
state(ref)             -> authored state value
digest(value)          -> storage payload digest
become                 -> physical output and template validation
observe / spawn        -> transaction checks
actor selector         -> template selection and proof
output <- self         -> exact script preservation
```

No Argent construct reaches `compile_contract_ast()`.

## Direction

- Parse each construct once.
- Use Silverscript AST nodes instead of Argent strings for ordinary code.
- Resolve scopes and references by identity, not by name-based rescans.
- Keep state and generated-field provenance typed until final lowering.
- Construct Silverscript AST directly. Do not generate, edit, or reparse Sil
  source fragments.
- Build artifacts from the resolved model and compiled Sil ABI. Do not infer
  artifact data from rendered `.sil`.
- Let Silverscript own formatting and compilation of the final AST.

## Remaining outline

1. Map these objects to the current parser, model, body lowerer, emitter, and
   artifact code.
2. List the current string fields, token scans, span edits, reparsing, and
   duplicated semantic paths.
3. Define a sequence of small migrations that keeps the compiler working after
   each commit.
4. Define expected identity changes, parity tests, runtime tests, and security
   review.
5. List the old types and helpers that must be removed at completion.
