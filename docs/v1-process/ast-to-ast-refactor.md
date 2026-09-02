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
    name: ModuleName,
    source: SourceFile,
    imports: Vec<Import>,
    constants: Vec<ConstDecl>,
    states: Vec<StateDecl>,
    functions: Vec<FunctionDecl>, // global fns
    actors: Vec<ActorDecl>,
    actor_enums: Vec<ActorEnumDecl>,
    apps: Vec<AppDecl>,
}

pub struct SourceFile {
    path: PathBuf,
    text: String,
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

Each module identifies its source file and retains its source text. Authored
nodes carry spans within that source. Compiler passes carry the module context
when they use those spans. Generated nodes use synthetic spans. Source text is
used for diagnostics, not as a semantic node.

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

## Migration direction

- Start in [`syntax`](../../src/compiler/syntax/mod.rs). Keep Argent
  declarations in the combined AST, but replace stored strings with
  Silverscript type, expression, and statement nodes where the code is ordinary
  Silverscript.
- Parse every source construct once. Keep its source location for diagnostics,
  but do not keep source text as its semantic representation.
- Change `id.co_spent()` to `co_spent(id)`. The new form is a normal
  Silverscript call and can appear in entries, global functions, and actor
  functions.
- Replace the temporary `For` node in
  [`syntax::body`](../../src/compiler/syntax/body/mod.rs) with a nested
  `sil::Statement`. Keep `Block` and `If` in the Argent entry tree because they
  may contain `become`. Keep `Become` as an Argent operation.
- Make `ResolvedProgram` hold the meaning of the selected app. Resolve names,
  scopes, input references, routes, state layouts, and witnesses to stable
  identities before code generation starts.
- Move any remaining semantic decisions out of
  [`emitter`](../../src/compiler/codegen/emitter.rs). Code generation should use
  the resolved program instead of searching source text or rebuilding model
  facts.
- Adapt the typed state, input-reference, route, and witness code in
  [`codegen::sil`](../../src/compiler/codegen/sil) to read AST nodes and produce
  AST nodes. Keep the source-state and generated-field origins typed until the
  final physical state is built.
- Build one `sil::ContractAst` for each actor. Do not generate a Silverscript
  source fragment and then parse it again.
- Give the same final contract AST to Silverscript for both formatting and
  compilation. The generated `.sil` file and bytecode must never follow
  separate code paths.
- Build the Argent artifact from `ResolvedProgram` and the compiled Sil ABI. Do
  not recover artifact facts from formatted `.sil` source.
- Remove the old token scans, source-span edits, parsing wrappers, repeated
  parsing, and text-emission helpers when their AST replacements are in use.
- Keep the compiler working after each migration commit. Compare generated
  output, explain any contract-identity change, run runtime tests, and finish
  with a security review of the complete lowering path.
