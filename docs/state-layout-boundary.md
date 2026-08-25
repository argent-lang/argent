# Argent State Representation Boundary

Status: implementation guide  
Implementation baseline: `global_fn_namespace` -> `actor_functions`, 2026-08-25  
Parallel branch: `entry-ranges-part1` remains paused until this boundary is stable

## 1. Purpose

This change establishes a strict boundary between the state that Argent users define and the physical state that generated Silverscript contracts store and validate.

The immediate goals are:

- make compiler-owned route fields absent from the Argent source model;
- centralize every conversion between user state and physical contract state;
- let layout-aligned codegen use Silverscript's native `State` object directly;
- replace the special meaning of `self.state` with an explicit `become self` transition;
- preserve the current route security rules and physical state encoding;
- make future state-layout changes local and auditable.

This is primarily an internal compiler architecture change. Except for replacing `self.state` with `self` in exact continuations, it should not change Argent application semantics, artifact-visible user state, or the runtime's physical state encoding.

The invariants and semantic rules in this guide are normative. Rust type names, module names, enum variants, and function signatures are illustrative: an implementation may choose a cleaner API so long as it preserves the boundary and all conformance requirements.

### 1.1 Implementation baseline and branch order

Implementation continues over two stacked branches rather than directly from `master`.

1. **`global_fn_namespace`** isolates global-function lowering. Global parameters and locals use the `gen__glob_` namespace; unresolved bare identifiers are rejected instead of capturing contract fields; and Sil's AST classifies bindings and references so exact-span rewrites preserve the original text, comments, and formatting. The branch currently pins Sil commit `09166e5` from `function-ast-api`, pending [Silverscript PR #222](https://github.com/kaspanet/silverscript/pull/222). See [Argent PR #49](https://github.com/argent-lang/argent/pull/49).

2. **`actor_functions`** adds actor-scoped parsing and namespace validation, emits a helper only in its owning actor contract, gives function signatures authored state layouts, supports direct state-returning helpers, and indexes actor functions in the VS Code extension. Its latest state-related changes are tactical repairs: entry state parameters are forced to remain authored, direct state-returning calls receive special recognition, results are preserved as authored locals, and route materialization has a direct-call exception. These cases demonstrate the missing boundary; they are not part of the target design and should be deleted as the contract-wide representation decision takes over. See [Argent PR #50](https://github.com/argent-lang/argent/pull/50).

A global function is emitted separately into every applicable actor contract. Consequently, the same authored state type may lower differently for the same function source in different compiled actor contexts. Actor functions are emitted only in their owner, but use the same per-contract layout decision. Representation must therefore be chosen during per-contract lowering, not stored as an intrinsic property of a source function or inferred from an initializer's syntax.

`entry-ranges-part1` stays paused through this series. Its element-wise state-array problem must later consume the shared scalar, fixed-array, and dynamic-array representation rules; it must not introduce range-specific state conversions.

### 1.2 Reading this guide

- Sections 2 through 5 define the normative semantics, boundary, and principles.
- Section 6 is a temporary migration sequence for the pending branches; its compatibility assertions are not permanent architectural rules.
- Sections 7 through 10 define migration checks and end-state conformance.

## 2. State representations

The compiler should distinguish three representations. Keeping all three explicit avoids confusing route-field lowering with virtual-state expansion.

### 2.1 Source state

Source state is the typed state visible to Argent code. It contains only fields declared by the user and preserves their source-level types.

For example:

```rust
state AccountState {
    byte[32] owner;
    int balance;
}
```

`AccountState` is source state. Users can read its fields, construct values of this type, and pass them through entry parameters or locals.

Source state never contains route templates, route-family digests, template witnesses, or any `gen__*` field.

### 2.2 Storage payload

Storage payload is the on-chain representation of the user-defined state before route fields are added.

For ordinary states, source state and storage payload have the same layout. For an expanded state, source values can contain structured virtual fields while the storage payload contains their digest representation. That source-to-storage transformation already has its own semantics and must remain independent of route-layout lowering.

The storage payload still represents only application state. It contains no route fields.

### 2.3 Physical state

Physical state is the exact state layout embedded in a generated Silverscript contract. For the active contract it is represented by Silverscript's `State` object.

Conceptually:

```text
physical state = compiler-owned route context + storage payload
```

The layout can remain flat in generated SIL. The separation is semantic and architectural; it does not require a nested wire encoding.

### 2.4 Identity conditions

There are two independent layout relations:

```text
source state  -> storage payload  -> physical state
               expansion lowering    route lowering
```

Each relation can be either identity or transformed.

- Source and storage are identical when no virtual expansion changes the representation.
- Storage and physical are identical when the actor has no compiler-owned route fields.
- Source and physical are identical only when both relations are identity.

Direct source-level use of SIL `State` requires source and physical state to be identical, so both relations must be identity. It also requires nominal source identity: the value must represent the same `SourceStateId` as the source type being requested. Two actors that share one source state can satisfy this condition; two distinct state declarations with identical fields cannot. Within an emitted contract, the target physical layout must also be compatible with that contract's active `State` type. Storage-level pass-through to a target physical representation requires only storage-to-physical identity; whether that representation is `State` or a named struct is a separate target/active compatibility decision. The usual active, no-route, non-expanded case satisfies all conditions.

## 3. Language semantics

### 3.1 Ordinary successor construction

An ordinary route constructs new user state:

```rust
become next <- Account(next_state);
```

Its semantics are:

1. Resolve the target actor and authorized output handle.
2. Interpret `next_state` as the target's source state.
3. Lower it to the target's storage payload if expansion requires it.
4. Materialize the target's physical state by adding the compiler-planned route context.
5. Validate the output with the strongest applicable Silverscript template builtin.

User code controls only step 2. The compiler exclusively controls steps 3 and 4.

### 3.2 Exact self-continuation

The exact continuation syntax is:

```rust
become self;
```

For a named route in a larger `become` set, the equivalent form is:

```rust
become next <- self;
```

and inside a block:

```rust
become {
    next <- self,
    peer <- Peer(next_peer),
};
```

`self` in this position is not a general expression. It is a dedicated successor form meaning:

> The selected successor output has exactly the same script public key as the active input.

Under the collision-resistance assumption of P2SH, this preserves the exact contract template and complete physical state, including all user fields and compiler-owned route fields.

It lowers directly to a comparison between the successor output's script public key and the active input's script public key. It must not reconstruct a state object and must not be expressed internally as `CurrentActor(self.state)`.

`become self` does not constrain the output's native KAS value. The existing explicit output-value policy remains unchanged. An entry that wants to preserve value must still state, for example:

```rust
require(next.value == self.value);
```

### 3.3 Resolution rules for `self`

- `become self;` is valid only when exactly one emitted singleton output handle permits the current actor. Other emitted outputs do not make it ambiguous.
- `output <- self` explicitly selects the output handle and is valid only when that output is singleton and permits the current actor.
- Ranged output handles cannot use exact self-continuation until the language defines element selection for that case.
- `self` is invalid as a local value, function argument, state constructor value, observed target, spawn target, or ordinary expression.
- Exact self-continuation is valid regardless of whether route fields exist. It preserves the physical script without inspecting its layout.

### 3.4 Removal of `self.state`

`self.state` should cease to be a source-level value. Its only important use was unchanged continuation, and `become self` expresses that operation more accurately and compiles more directly.

Changed transitions remain explicit. Users read the current fields and construct the new state:

```rust
AccountState next_state = {
    owner: owner,
    balance: balance + amount,
};
```

An occurrence of `self.state` should produce a targeted diagnostic:

```text
`self.state` is not a value; use `become self` for an unchanged continuation,
or construct the successor state explicitly from its fields
```

Removing `self.state` also avoids an otherwise ambiguous question: whether it denotes source state, storage payload, or complete physical state.

## 4. Target architecture

### 4.1 Semantic IR: successor intent is explicit

The parsed and resolved model must not infer exact preservation by comparing source text such as `compact_expr(&route.state) == "self.state"`.

Use an explicit successor form:

```rust
// Illustrative only.
enum Successor {
    ExactSelf {
        span: Span,
    },
    Constructed {
        target: ActorTarget,
        state: SourceExpr,
    },
}

struct ResolvedRoute {
    output: OutputHandleId,
    successor: Successor,
}
```

Parsing may initially retain spans, but semantic resolution must produce a structured successor before SIL lowering begins. Output inference for `become self;` belongs in semantic resolution, where the entry's `emits` model is available.

### 4.2 One lowering environment per emitted contract

A source actor can compile in more than one application context. Route fields are therefore properties of a compiled actor context, not merely of a source state declaration. Likewise, a global function is instantiated in more than one emitted contract, so its SIL representation cannot be fixed on the source function.

After route planning, build one immutable lowering environment for each emitted contract. It owns the active actor's source/storage/physical relations, the representation selected for every source state type used by entries or emitted functions, and the physical plan for every referenced input or output target:

```rust
// Illustrative only. Preserve the relations, not necessarily these names.
struct ContractStateLowering {
    active: ContractStateLayout,
    source_representations: Map<SourceStateId, SourceRepresentationPlan>,
    target_physical: Map<PhysicalTargetId, TargetPhysicalPlan>,
}

struct SourceRepresentationPlan {
    source_to_storage: SourceStorageRelation,
    sil: SourceSilRepresentation,
}

enum SourceSilRepresentation {
    ActiveState,
    Named(SilType),
}

struct TargetPhysicalPlan {
    source: SourceStateId,
    storage_to_physical: StoragePhysicalRelation,
    physical: PhysicalStateLayout,
    sil_type: SilType, // `State` or a named physical struct in this contract
}

struct ContractStateLayout {
    actor: CompiledActorId,
    source: SourceStateLayout,
    storage: StorageStateLayout,
    physical: PhysicalStateLayout,
    source_to_storage: SourceStorageRelation,
    storage_to_physical: StoragePhysicalRelation,
}

enum SourceStorageRelation {
    Identity,
    Expanded(ExpansionMap),
}

enum StoragePhysicalRelation {
    Identity,
    Augmented {
        generated_fields: Vec<GeneratedField>,
        storage_to_physical: FieldMap,
    },
}
```

`PhysicalTargetId` identifies a concrete actor target or a dynamic target capability with a known physical layout. A dynamic capability is valid only when all of its variants have one canonical physical layout and SIL type. Reject a capability whose variants have incompatible route cuts; selector-discriminated materialization is outside this design. Target plans are actor-context keyed, not source-state keyed: two actors can own the same source state while carrying different route cuts. Conversely, a different template can use `State` when its complete physical layout is identical to the active contract's layout.

Physical compatibility is semantic, not merely byte compatibility. It compares ordered field identities, SIL types, and generated-field roles. Two layouts with the same packed widths but different route-field meaning are not compatible.

The target plan therefore owns one further compatibility decision:

```text
target physical layout == active physical layout -> use State
otherwise                                        -> use a named physical struct
```

This decision applies to consumed and observed inputs, ordinary and dynamic emit targets, observed outputs, spawns, and linked actors. It remains independent of the target's source representation and of the proof used to authenticate its template.

Use stable field identities in `FieldMap`. Ordinary codegen must not calculate physical positions using string names, generated-field counts, or ad hoc offsets.

The environment should be the single source of truth for:

- the generated contract `State` declaration;
- input-state builtin result types;
- physical types for every concrete or dynamic input and output target;
- entry parameter lowering;
- global and actor function parameters, results, locals, constructors, and array elements;
- user-field projection;
- output-state materialization;
- artifact physical layout metadata;
- the two identity decisions—storage/physical pass-through and source/physical identity—and the separate target/active physical compatibility decision.

### 4.3 A dedicated SIL state boundary

Add a small module, conceptually `src/compiler/codegen/sil/state_boundary.rs`, that owns all use of physical layouts in body lowering.

Its public responsibilities should be narrow:

```rust
// Illustrative API surface.
fn bind_input_state(...) -> SourceStateAccess;
fn require_authored_value(...) -> AuthoredStateExpr;
fn lower_authored_to_storage(...) -> StorageStateExpr;
fn materialize_output_state(...) -> PhysicalStateExpr;
fn validate_output_state(...) -> SilStatement;
fn preserve_exact_self(...) -> SilStatement;
```

Suggested result types:

```rust
// Illustrative representation categories.
enum SourceStateAccess {
    Authored {
        expr: SilExpr,
        sil_type: SilType, // `State` only with source/physical identity and active-layout compatibility
    },
    View {
        physical_expr: SilExpr,
        source_type: SourceStateId,
        fields: FieldMap,
    },
}

struct AuthoredStateExpr { /* complete source value */ }
struct StorageStateExpr { /* lowered payload, no route fields */ }

struct PhysicalStateExpr {
    expr: SilExpr,
    sil_type: SilType,
    layout: PhysicalStateLayoutId,
}
```

The concrete API should also carry opaque evidence such as `AuthenticatedPhysicalInput`, `ExpansionOpenings`, `PhysicalMaterializationPlan`, and `GeneratedFieldSource`. A materialization plan maps every generated field exactly once to current compiler context, an authenticated input, linked template context, or a route-transition derivation. Ordinary codegen must not be able to construct these proofs or a trusted physical value directly.

`SourceStateAccess::Authored(State)` does not expose route fields to Argent. It records nominal source identity, both identity relations, and compatibility with the active contract's `State` type. `SourceStateAccess::View` supports projections only; `require_authored_value` must either materialize a complete source value under the expansion rules or report that the required preimages are unavailable.

### 4.4 Physical layout is confined to boundary operations

Within Argent-to-SIL codegen, physical layout should be relevant only in these areas:

1. contract `State` declaration and constructor parameters;
2. results of `readInputState` and `readInputStateWithTemplate`;
3. arguments to `validateOutputState`, `validateOutputStateWithInputTemplate`, and `validateOutputStateWithTemplate`;
4. exact self-preservation, which bypasses layout conversion and compares script public keys;
5. artifact and ABI emission, which describes the physical layout for the runtime.

Expression lowering, route authorization, source type checking, local state construction, and user-field access must not inspect route fields.

### 4.5 Direct `State` use on the fully aligned path

Three fast-path questions must remain distinct:

```rust
fn storage_is_physical_identity(layout: &ContractStateLayout) -> bool {
    layout.storage_to_physical == StoragePhysicalRelation::Identity
}

fn source_is_physical_identity(layout: &ContractStateLayout) -> bool {
    layout.source_to_storage == SourceStorageRelation::Identity
        && layout.storage_to_physical == StoragePhysicalRelation::Identity
}

fn target_uses_active_state(
    active: &PhysicalStateLayout,
    target: &TargetPhysicalPlan,
) -> bool {
    target.physical.is_sil_compatible_with(active)
}

fn physical_can_serve_as_source(
    requested: SourceStateId,
    source: &SourceRepresentationPlan,
    target: &TargetPhysicalPlan,
    active: &PhysicalStateLayout,
) -> bool {
    requested == target.source
        && source.sil == SourceSilRepresentation::ActiveState
        && source.source_to_storage == SourceStorageRelation::Identity
        && target.storage_to_physical == StoragePhysicalRelation::Identity
        && target_uses_active_state(active, target)
}
```

`storage_is_physical_identity` means that an already-lowered storage payload can be passed to physical-state builtins without adding route fields. It does not mean that physical `State` is a valid authored source value.

`source_is_physical_identity` establishes that a layout's physical value has its own authored layout. `target_uses_active_state` establishes that the physical value may use this emitted contract's SIL `State` type. `physical_can_serve_as_source` additionally requires nominal identity with the requested source state and confirms that the contract-wide source representation actually selected active `State`. Keeping the target's source identity inside `TargetPhysicalPlan` prevents a caller from pairing a physical target with an unrelated source type. Only that complete predicate permits `SourceStateAccess::Authored(State)` and makes all source-level operations no-ops:

```text
project_to_source(State)     -> State
lower_source_state(State)    -> State
materialize_physical(State)  -> State
pass_to_builtin(State)       -> State
```

SIL `State` denotes the active emitted contract's physical layout, not its template identity. A different template's physical value may therefore use `State` when its complete physical layout is compatible with the active layout. That physical compatibility is not enough to make the value authored source state: direct authored use additionally requires the target's source-to-storage and storage-to-physical relations to be identity.

An expanded state without route fields is the important counterexample:

```text
source -> storage: transformed
storage -> physical: identity
```

Its physical `State` contains the digest storage layout. It may pass directly between storage-level and physical-level operations, but it cannot serve as the authored source value. Source reads still require expansion projection, and source values still require expansion lowering before they become physical `State`.

The lowering environment computes both identity facts and target compatibility once. Codegen must not contain repeated `has_route_fields()`, `has_expansion()`, or structural layout-comparison branches.

When storage-to-physical is `Augmented`, the boundary additionally projects or materializes the compiler-owned route context. The rest of body lowering follows the same semantic path.

### 4.6 Projected views, authored values, and function boundaries

The compiler must distinguish a projected user view from a materialized authored value.

- A **projected view** supports field access without reconstructing the whole source value. For example, `peer.balance` may project `balance` from a validated physical input. This is sufficient when the expression is consumed as that field.
- An **authored value** is a complete source-level value. It can be bound as a local, passed to a function expecting `PeerState`, returned from a function, stored in an array, destructured, or used as a successor source expression.
- A **storage payload** and a **physical value** are boundary representations and are not authored values unless the applicable identity predicates prove they coincide.

Converting a whole projected peer state to an authored value is a real operation. For ordinary fields it can construct the source struct from projections. For expansion-backed fields it requires validated payload preimages according to the existing expansion rules. A digest can support equality or storage validation, but cannot reconstruct the authored nested value. Codegen must reject an unavailable whole-value conversion rather than pretending that the physical digest layout has the source type.

Function calls are representation boundaries even though they are not physical builtins. Each emitted function instance receives a per-contract lowering plan for every state-valued parameter, result, local, destructuring binding, constructor, and array element. The call and callee must agree on that plan.

The following invariants are mandatory:

1. Entry parameter to function parameter, authored local to function parameter, and function result to local use the same authored-value contract.
2. A consumed or observed physical state may be passed as a whole authored argument only after authored-value reconstruction has produced that value; direct field projection alone is insufficient.
3. Nested calls compose the declared representation plan. No call-site decision depends on whether the initializer happens to be a direct call.
4. State-returning calls are evaluated exactly once before any projection, expansion lowering, route materialization, or repeated element access.
5. The same rules apply recursively to scalar state values, fixed arrays, and dynamic arrays. Array representation is an element-wise consequence of the shared plan, not a separate range feature.
6. In the fully aligned case only—both identity relations plus target/active physical compatibility—a coherent contract-wide lowering may represent authored state values as SIL `State`. In all other cases, authored and physical categories stay distinct even if a particular expression can be projected cheaply.

Global functions are planned independently for each contract into which they are emitted. Actor functions use their owning contract's plan. This preserves the namespace and ownership semantics of the pending branches while removing their initializer-shape special cases.

Actor functions may capture ordinary actor fields, but cannot directly project through a virtual field whose physical representation is only a digest. For example, `strategy.hunger` inside an actor function is rejected when `strategy` is expansion-backed. Argent lowering must report this against the field access span and identify the owning actor function; it must not defer the failure to Silverscript type checking. The opened authored value must instead be supplied explicitly as a function parameter. Adding hidden opening parameters or entry-context inlining is outside the initial implementation.

### 4.7 Structured SIL type rewriting

On the fully aligned path, direct `State` use must be coherent across the entire emitted contract. A state spelling such as `LocalState` may need to become `State` in:

- function parameters and result types;
- ordinary and destructuring local declarations;
- fixed- and dynamic-array element types;
- constructors and other genuine type-expression occurrences.

A textual replacement is unsafe: the same spelling may denote a variable, field, function, constructor, type, comment, or string. Argent intentionally does not parse the complete Sil function language itself.

Use Sil's structured function API—`parse_function_ast(...)`, `visit_function_mut(...)`, and classified `visit_name(name, NameKind, span)` traversal—for exact-span edits that retain source formatting and comments. Extend that API, if necessary, with typed visitation of `TypeRef` occurrences and their source spans, including constructor and type-expression positions. The AST already owns structured type references and `type_span` data; Argent should expose those through the Sil visitor rather than adding a lexer or duplicating Sil's grammar.

Entry bodies may continue through Argent's structured lowering for Argent-specific statements. Plain global and actor function bodies require the standalone Sil AST traversal because their full language belongs to Sil.

### 4.8 Builtin selection is independent of layout conversion

Template authentication and state representation answer different questions and should remain separate.

First classify template proof:

```rust
// Illustrative only.
enum TemplateProof {
    CurrentTemplate,
    BoundInputTemplate(InputRef),
    WitnessedTemplate(TemplateWitnessRef),
}
```

Then select the builtin:

| Template proof | Output builtin |
| --- | --- |
| Current template | `validateOutputState` |
| Authenticated bound input | `validateOutputStateWithInputTemplate` |
| Explicit template witness | `validateOutputStateWithTemplate` |

The chosen builtin always receives a `PhysicalStateExpr`. The selected target plan decides whether its SIL type is `State` or a named physical struct; materialization decides how to produce it; template proof decides how to authenticate the surrounding template. None of these decisions implies either of the others.

`Successor::ExactSelf` is a separate path and uses none of these state-validation builtins.

### 4.9 Current compiler mapping

The existing implementation already contains most required concepts, but they are distributed across large lowering functions:

- `src/compiler/model/layout.rs` currently contains fixed-width layout facts and is the natural home for typed layout plans, or for a sibling `state_layout.rs` module.
- `src/compiler/codegen/emitter.rs` emits physical state structs, reads peer states, selects input builtins, and currently recognizes exact self through a textual `self.state` comparison.
- `src/compiler/codegen/sil/body.rs` owns state bindings, state-expression materialization, route lowering, and output builtin selection.
- global and actor function lowering must consume the same per-contract state plan; it must not preserve the pending branches' direct-call and authored-local exceptions.
- `src/compiler/codegen/emitter/tests.rs`, `tests/body_lowering_scopes.rs`, and generated example fixtures provide the existing test surfaces.

The target is not a second codegen implementation. It is a small layout plan plus one boundary adapter used by the existing emitter and body lowerer.

## 5. Architectural principles

1. **User state has no route fields.** Compiler-owned fields are never source members and never accepted in source constructors.

2. **Conversions are explicit in compiler lowering.** Argent may insert a conversion without source syntax, but every source/storage/physical transition goes through projection, expansion lowering, or materialization in the typed boundary.

3. **Layouts are planned once.** The compiler computes one lowering environment per emitted contract, including its active physical layout, the representation of each used source state type, and every referenced target's physical layout. Codegen consumes that environment and does not rediscover it.

4. **Field identity is structural.** Map fields by stable IDs, not by concatenated names, generated prefixes, or positional arithmetic scattered through codegen.

5. **Exact self is semantic, not an optimization heuristic.** `become self` produces `Successor::ExactSelf` and lowers directly to script-public-key equality.

6. **Template proof and state layout stay orthogonal.** Choosing a template-validation builtin does not decide how state is represented, and materializing state does not decide how its template is authenticated.

7. **The aligned path uses SIL naturally.** If the relevant layouts are identical, use Silverscript's `State` object directly rather than manufacturing an equivalent struct.

8. **Physical encoding remains stable unless deliberately changed.** This refactor preserves physical field order, encodings, and generated witness recipes. Coherent aligned lowering may deliberately change generated SIL parameter type names, dispatch tags, scripts, template hashes, and artifact IDs; every such change must be identified and reviewed.

9. **No fallback textual recognition.** Codegen must not recognize special transitions by compacting or comparing source strings.

10. **Boundary leakage is prevented by construction.** Keep physical-layout types private to the boundary or expose only opaque typed handles and operations. Prefer Rust visibility and type checking over a repository text-scanning allowlist.

11. **Function representation is contextual, not syntactic.** Choose it once per emitted contract and state type. Never infer it from an initializer being a direct call, a parameter originating at an entry, or a result immediately feeding a route.

12. **Evaluation behavior is semantic.** A state-producing expression is evaluated once and in its original order even if conversion projects multiple fields, lowers expansion payloads, or materializes multiple physical components. Conversion must not hoist work across a conditional or short-circuit boundary.

## 6. Gradual implementation plan

This section is a migration plan, not a permanent API specification. The sequence is designed as independently reviewable commits on top of `actor_functions`, itself stacked on `global_fn_namespace`. Preserve the two branches' user-facing function features and namespace guarantees, but do not preserve their tactical representation special cases merely because fixtures currently encode them.

### Commit 1: Characterize the existing representation behavior

Add focused fixtures before restructuring code. Characterize the stacked-branch behavior as evidence, while marking initializer-shape-dependent SIL as migration debt rather than golden architecture.

Include at least:

- one actor with no route fields and no expansion;
- one actor with generated route fields;
- one state with virtual expansion;
- one same-actor changed-state transition;
- one exact continuation using the current `CurrentActor(self.state)` form;
- one foreign transition using each applicable template-validation path;
- one consumed and one observed input state;
- one global state-valued function emitted into two actor contexts with different layout relations;
- one actor state-valued helper;
- direct and nested state-returning calls, including a call whose result is routed;
- scalar, fixed-array, and dynamic-array state parameters and results where supported.

Record for each fixture:

- generated SIL;
- emitted state-struct field order;
- chosen read/validate builtin;
- artifact user state layout;
- artifact physical template plan;
- compiled template hash.

Add narrow tests around the current `route_validation_kind` decisions and assert that state-returning calls are evaluated once. This commit must not change production code or generated output.

Exit condition: all baseline vectors are checked in, tactical expectations are labeled, and `./check.sh --full` is clean on the stacked branch.

### Commit 2: Introduce typed layout plans without changing codegen

Add the two layout relations, stable field IDs, and typed lookup APIs. The concrete Rust model may differ from the illustrative types in section 4.

Build plans from the existing source/storage state model and route planner. Initially, compare each plan against the existing ad hoc output:

- assert identical physical field order;
- assert identical generated field roles;
- assert identical storage widths and field encodings;
- assert one representation decision for each state type within each emitted contract;
- assert one physical layout and SIL type for every referenced actor or dynamic target;
- reject dynamic targets whose variants do not share one canonical physical layout and SIL type;
- require nominal `SourceStateId` equality before a physical value serves directly as authored state;
- cover actors that share one source state but carry different route cuts;
- cover a different template whose physical layout can use the active `State`;
- distinguish byte-compatible layouts whose generated fields have different semantic roles;
- assert global functions receive the destination contract's decision rather than a source-global decision.

Keep existing codegen authoritative for this commit. The new plan runs in parallel only as a checked model. Authored values remain in
their named source types, and non-active targets retain named physical types; record target/active compatibility without selecting the
equivalent-`State` optimization yet. Existing codegen may still contain earlier `State` equivalence shortcuts during this parallel phase;
do not reproduce them as legacy policy in the new plan merely to make the temporary type choices match.

Exit condition: every emitted contract has one lowering environment and characterization outputs remain byte-for-byte unchanged.

### Commit 3: Add the SIL state boundary and migrate input reads

Introduce the boundary module and opaque authored/projected/physical categories. Use Rust privacy so ordinary expression and function lowering cannot inspect physical fields or construct a physical value directly.

Move consumed and observed input lowering behind `bind_input_state`:

- preserve the existing security rule for `readInputState` versus `readInputStateWithTemplate`;
- select the result type from the authenticated target's physical plan;
- keep authenticated physical `State` distinct from authored source values until the equivalent-`State` optimization is selected;
- return a direct authored binding only when the input already uses its named source type and both layout relations are identity;
- otherwise return a projected binding with a stable field map;
- distinguish direct field projection from whole authored-value materialization;
- require the existing validated expansion preimages before materializing an expanded authored value;
- keep `.value` and covenant-id handling outside state projection.

Do not migrate output materialization yet.

Exit condition: all existing negative template tests still fail and no input read call site chooses a physical state type independently.
Generated input SIL should remain stable where the old type choice agrees with the plan; when an earlier equivalent-`State` shortcut
disagrees, use the plan's named physical type and review the resulting SIL and artifact changes rather than reproducing the shortcut.

### Commit 4: Migrate all state-valued expressions and function boundaries

Replace `entry_param_sil_type` checks, direct-call recognition, authored-local preservation, and route-call exceptions with the per-contract plan. Migrate, as one coherent representation surface:

- entry parameter -> function parameter;
- authored local -> function parameter;
- consumed or observed physical state -> projected field or materialized authored function parameter;
- function result -> local -> `become`;
- nested global and actor function calls;
- function parameters and results, local and destructuring declarations, assignments and reassignments, constructors, and genuine type expressions;
- scalar state types plus fixed- and dynamic-array state element types;
- single evaluation of every state-returning call.

For plain global and actor function text, use Sil's AST and exact source spans. Extend its visitor with typed `TypeRef` visitation if the current API cannot identify every type and constructor occurrence. Do not add textual `LocalState -> State` replacement or an Argent-side Sil lexer.

In the fully aligned case, where both relations are identity and the relevant physical plan uses active `State`, apply a coherent contract-wide lowering: parameters, results, compatible locals, arrays, constructors, and consumed inputs may all use SIL `State` directly. Do not emit a duplicate source-named struct solely for that state. In augmented, expanded, or physically incompatible cases, retain the planned authored representation and cross to the selected physical type only through the boundary.

Generated SIL changes are expected here where the tactical actor-function representation is replaced or a fully aligned contract switches coherently to `State`. Review those diffs semantically; do not require byte-for-byte preservation of the tactical form.

Implement this surface as separate reviewable commits for scalar/function lowering and array lowering. Array type substitution does not imply element-wise conversion: converting a dynamic array requires a compiler-known maximum, and an unbounded conversion must be rejected. Any required Sil visitor extension should remain an isolated dependency commit.

Exit condition: every call and callee agrees by construction; no result representation depends on initializer shape; expanded no-route states never masquerade as authored `State`; namespace isolation and actor ownership tests from PRs #49 and #50 still pass; and state-producing calls evaluate once.

### Commit 5: Migrate successor materialization and output validation

Move `lower_state_expr_for_actor`, `lower_state_expr_for_layout`, generated-field insertion, and output builtin calls behind the boundary API.

The new flow is:

```text
source expression
    -> storage payload
    -> physical state expression
    -> template-authenticated output validation
```

Requirements:

- when both relations are identity, source `State` passes directly through materialization;
- when only storage-to-physical is identity, source expansion lowering must first produce the digest storage representation used by physical `State`;
- augmented materialization obtains every generated field from the route transition plan;
- output type selection comes from the selected target's physical plan rather than its source state name;
- user constructors cannot initialize or override generated fields;
- family packing and transition-specific route changes remain compiler-controlled;
- builtin selection remains identical to the baseline.

Keep the legacy exact-continuation syntax working through the existing semantic classification for one more commit, but route its result through a dedicated exact-preservation operation.

Exit condition: no output call site manually assembles physical fields or independently selects a physical SIL state type.

### Commit 6: Add `become self` and remove `self.state`

Add the dedicated successor syntax and IR variant.

Support:

```rust
become self;
become next <- self;
become { next <- self, peer <- Peer(next_peer) };
```

Perform output-handle inference and target compatibility checks during semantic resolution. Replace textual exact-self recognition in `route_validation_kind` with structured matching on `Successor::ExactSelf`.

Lower exact self directly to the existing script-public-key comparison. Do not create a state expression or call a state-validation builtin.

Remove `self.state` from valid members and update examples, design notes, syntax highlighting, diagnostics, and fixtures. Keep a specific error for old syntax rather than allowing it to fail later in Silverscript.

Exit condition: exact-self generated SIL and template hashes match the baseline exact-continuation path, while every non-`become` use of `self` and every `self.state` use is rejected.

### Commit 7: Remove legacy paths and lock conformance

Delete:

- textual `self.state` recognition;
- duplicated physical state type selection;
- manual route-field insertion outside the boundary;
- dead source-state structs emitted only because old codegen could not use `State`;
- direct-call, entry-parameter, authored-local, and route materialization exceptions from `actor_functions`;
- transitional assertions comparing old and new layout logic.

Tighten Rust visibility so physical-layout construction and inspection are private to layout planning, the SIL boundary, contract-state emission, artifact emission, and runtime construction. Ordinary body and function lowering should receive opaque handles or typed operations, not a field list. Add focused compile-time/API tests where practical; do not add a generic repository text-scanning allowlist.

Regenerate tracked fixtures once, review every generated SIL and artifact diff, and run the complete conformance suite below.

Keep `entry-ranges-part1` paused during this commit. Document the shared array element rules it must consume when later resumed.

Exit condition: `./check.sh --full` passes; every representation switch, including functions and arrays, is reachable through the typed boundary; documentation describes `become self`; and generated Sil ABI or contract-identity changes, if any, are explicitly enumerated rather than incidental.

## 7. Sanity checks during the migration

Run the standard fast checks after every meaningful edit and the full fixture regeneration before every commit that changes generated code:

```text
cargo fmt --check
cargo test
./check.sh
./check.sh --full
```

Run `./check.sh` from Argent Playground after every commit that changes generated contracts, artifacts, runtime construction, or source syntax, and before completing the series.

The following invariants deserve explicit attention throughout the sequence.

### 7.1 Generated-code stability

- Commits 1 through 3 should produce no generated SIL or artifact diffs.
- Commit 4 may intentionally change function signatures, local types, constructors, and call sites when replacing tactical lowering with the coherent per-contract plan. Every diff must correspond to a recorded representation decision.
- Commit 5 should ideally be byte-for-byte stable. Any formatting-only diff should be reviewed separately from semantic changes.
- Commit 6 may change source fixtures, but the exact-self generated SIL should remain identical to the old exact-continuation output.
- Record template hashes before the series and compare them after every commit. A changed hash requires an identified generated-SIL cause.

### 7.2 Layout stability

- User field order and encoding do not change.
- Generated route field order, types, and roles do not change.
- User artifact schemas contain no generated route fields.
- Runtime physical construction still supplies every generated field exactly once.
- Expansion digests remain in their previous storage positions.
- An identity plan is never reported when source expansion or route augmentation makes the required representations differ.

### 7.3 Security-path stability

- `readInputState` remains restricted to the currently proven same-template input cases.
- Multi-actor and foreign reads retain template authentication.
- Same-template changed-state routes still use `validateOutputState`.
- Reusable authenticated input templates still use `validateOutputStateWithInputTemplate` only under the existing proof conditions.
- Other foreign routes still use `validateOutputStateWithTemplate`.
- Exact self uses script-public-key equality and no weaker comparison.

### 7.4 Source boundary stability

- User code can neither name nor initialize `gen__*` fields.
- User field references behave identically whether backed by direct `State` or a projected binding.
- `.value` remains transaction-output value, not a state field.
- `self.cov_id` and other `self` context members retain their current lowering.
- `become self` does not silently add a native-value constraint.

### 7.5 Runtime and artifact stability

- Existing artifacts still build equivalent UTXOs and transactions.
- Hidden witness recipes remain complete and ordered.
- Artifact dependency IDs and interface fingerprints change only if their documented inputs intentionally change.
- Generated dispatch tags, scripts, template hashes, and artifact IDs may change when an aligned entry signature deliberately lowers to `State`; record the cause of each change.
- Do not bump the artifact schema merely for internal Rust types. Bump it only if serialized artifact meaning or shape changes.

### 7.6 Function and evaluation stability

- Global namespace rewriting still changes only classified bindings and references; unresolved bare names never begin capturing actor fields.
- Actor functions remain emitted only in their owner and retain their existing namespace validation.
- A global function compiled into different actor contracts may have different lowered SIL state types, while keeping one authored signature and behavior.
- Callers and callees agree for state parameters, results, destructuring, constructors, and nested array elements.
- No direct-call or initializer-shape branch survives as a representation decision.
- A state-returning call is emitted once in its original evaluation position, then its bound result is projected or converted as needed.
- Conversion does not hoist indexed or state-producing expressions across conditional or short-circuit boundaries.

## 8. End-state test strategy

The finished change should be covered at seven levels.

### 8.1 Layout-plan unit tests

Test layout relation calculation without generating SIL:

- ordinary state, no route fields: both relations are identity;
- ordinary state with route fields: storage-to-physical is augmented;
- expanded state without route fields: source-to-storage is expanded, storage-to-physical is identity;
- expanded state with route fields: both transformations are present;
- every storage field maps to exactly one physical field;
- every generated physical field has a compiler role and no source field ID;
- field maps preserve declared ordering and packed widths.
- actors sharing a source state can retain different target physical layouts;
- a target uses active `State` exactly when its complete physical layout is compatible.
- nominally different source states remain distinct even when their field layouts match;
- byte-compatible physical layouts with different generated-field roles are not compatible;
- dynamic target variants with incompatible route cuts are rejected.

Add property-style tests for the core algebra:

```text
project_source(materialize_source(user, route)) == user
materialize_storage(storage, none) == storage    when storage-to-physical is identity
project_source(physical) == physical             only when both relations are identity
physical_fields == generated_fields + mapped_storage_fields
```

### 8.2 Parser and semantic tests

Positive cases:

- `become self;` with one compatible emitted output;
- `become next <- self;`;
- a multi-route block containing one exact-self route;
- exact self in an actor that has route fields.

Negative cases:

- ambiguous `become self;` with multiple singleton outputs that permit the current actor;
- exact self on a ranged output handle;
- `output <- self` when the output cannot become the current actor;
- `self` in an ordinary expression;
- `self` as a state constructor value;
- `self.state` anywhere;
- exact self attached to an observed or spawned output if that context does not denote the active actor continuation.

### 8.3 Function-boundary tests

Exercise both global and actor functions in aligned, augmented, and expanded contract contexts:

- entry parameter -> function parameter;
- authored local -> function parameter;
- validated consumed or observed state -> projected field and whole authored parameter;
- function result -> local -> `become`;
- nested function calls;
- state assignments and reassignments across compatible and incompatible representations;
- scalar, fixed-array, and dynamic-array state values;
- a state-returning call whose result feeds more than one projection, proving single evaluation;
- the same global function emitted into two contracts with different state-layout relations;
- expanded whole-value passage with valid payload preimages, and rejection when only a digest is available.
- direct capture of an expansion-backed actor field receives an Argent diagnostic at the field access, while an explicitly supplied authored parameter works.

Assert type occurrences structurally: parameters, results, locals, destructuring declarations, arrays, and constructors. Include decoy occurrences of the state spelling as a variable, field, function, comment, and string so an unsafe textual rewrite cannot pass.

### 8.4 Golden generated-SIL tests

Pin concise generated SIL for each layout/builtin combination. Prefer small purpose-built fixtures over relying only on large example snapshots.

Assert both positive and negative patterns:

- fully aligned paths contain direct source-level `State` use and no redundant generated struct;
- expanded states without route fields use `State` only as storage/physical representation, never as their authored source value;
- augmented paths contain the expected physical state type and generated fields;
- exact self contains the script-public-key comparison and contains no `validateOutputState*` call for that route;
- changed same-actor state contains `validateOutputState`;
- foreign paths contain the intended template-aware builtin;
- template-aware paths can still use `State` when the target physical layout matches the active layout;
- no generated SIL exposes a source member corresponding to a route field.

### 8.5 Silverscript compilation tests

Compile every golden fixture through Silverscript. This catches type mismatches that string snapshots miss, especially:

- passing `State` directly where a generated struct is expected;
- projecting an expanded or augmented state through the wrong type;
- constructing physical state fields in the wrong order;
- using a source struct where a builtin requires physical `State`.

### 8.6 Runtime end-to-end tests

For every conformance vector, use `argent-runtime` to build the input UTXO, successor outputs, entry arguments, and complete transaction. Execute the generated scripts where the existing test harness permits.

Check valid transitions and adversarial mutations of:

- one user field;
- one generated route field;
- the successor actor template;
- the template witness;
- the selected output handle or index;
- native value independently from state.

### 8.7 Boundary API tests

Constrain physical route-field knowledge with module ownership and Rust visibility to:

- layout planning;
- the SIL state boundary;
- contract state declaration emission;
- artifact/ABI emission;
- runtime physical-state construction.

Test the public boundary behavior through typed construction and conversion APIs. If a compile-fail test is useful, prove that ordinary body/function lowering cannot construct or destructure the physical type. A repository-wide text allowlist is unnecessary and too coupled to names; privacy should make bypasses fail to compile.

## 9. End-state conformance vectors

Each vector should record source, expected layout relations, expected SIL state type, expected builtin class, artifact expectations, and runtime mutations. Keep the initial assertions in focused tests and small fixtures under `tests/fixtures/state_layout/`.

The function-boundary coverage must include this matrix. `A` means fully aligned, `X` expanded source-to-storage, and `R` route-augmented storage-to-physical.

The `A` column's direct-`State` cases assume that the selected target physical layout is compatible with active `State`; otherwise the lowering environment selects a named representation despite the two identity relations.

| Value flow | A | X | R |
| --- | --- | --- | --- |
| Entry parameter -> function parameter | direct `State` permitted | authored value | authored value |
| Authored local -> function parameter | direct `State` permitted | authored value | authored value |
| Validated physical peer -> whole authored parameter | direct `State` only when both relations are identity and target/active physical layouts are compatible | requires validated preimages | materialize source fields; hide routes |
| Function result -> local -> `become` | direct through boundary | expansion lowering | route materialization |
| Nested function calls | one consistent contract plan | one consistent contract plan | one consistent contract plan |
| Fixed/dynamic arrays of state | apply `A` element plan | apply `X` element plan | apply `R` element plan |

Every row involving a state-producing call also asserts single evaluation.

### V1: Aligned same-actor state change

Setup: one actor, ordinary state, no route fields, no expansion.

Source transition:

```rust
become next <- Counter(next_state);
```

Expected:

- source-to-storage: identity;
- storage-to-physical: identity;
- compatible state values lower directly to `State`;
- output validation uses `validateOutputState`;
- no duplicate `Gen__CounterState`-style struct is emitted;
- changing the output state to a value other than `next_state` fails.

### V2: Aligned exact self

Setup: same as V1.

Source transition:

```rust
require(next.value == self.value);
become self;
```

Expected:

- output handle is inferred;
- generated code compares output and active-input script public keys;
- no state object is constructed for the route;
- no `validateOutputState*` builtin is used for the route;
- changing any user state field fails;
- changing the native value fails because of the explicit `require`, not because of `become self`.

Repeat without the value `require` and with the existing explicit unrestricted-value declaration. A value-only change must then remain permitted by the exact-self check.

### V3: Augmented exact self

Setup: current actor has one or more generated route fields.

Expected:

- storage-to-physical: augmented;
- `become self` still takes the exact script-public-key path;
- neither source-state projection nor physical materialization occurs;
- changing a user field fails;
- changing a generated route field fails;
- changing the template fails.

### V4: Augmented same-actor state change

Setup: current actor has generated route fields and constructs changed user state.

Expected:

- user expression contains only declared fields;
- materialization supplies current/target route fields from the route plan;
- output validation uses `validateOutputState`;
- the intended user-state change succeeds;
- omitted, reordered, or caller-substituted generated fields are impossible at the source level;
- a transaction-level mutation of any generated field fails.

### V5: Cross-actor route transition

Setup: actor `A` becomes actor `B`, and their route cuts differ.

Expected:

- the successor's source expression has `B`'s user type;
- materialization produces `B`'s physical layout;
- common route fields are preserved and changed route fields are derived from the `A -> B` transition plan;
- the builtin is `validateOutputStateWithInputTemplate` when an authenticated input proves `B`, otherwise `validateOutputStateWithTemplate`;
- using `A`'s physical layout or route cut for `B` fails.

### V6: Direct consumed input

Setup: the existing security model proves a consumed input has the current template, and both of its layout relations are identity.

Expected:

- input read uses `readInputState` under the current singleton-domain rule;
- the returned source-level user binding is direct `State` because both relations are identity;
- field access requires no copied user struct;
- applying the same direct read in a multi-actor domain is rejected by the compiler model or uses the template-aware builtin instead.

### V7: Template-authenticated consumed or observed input

Setup: peer or observed actor with an augmented physical layout.

Expected:

- input read uses `readInputStateWithTemplate`;
- body code sees only the peer's source fields;
- generated route fields are not addressable;
- a wrong template witness fails before its physical state is trusted;
- projection returns the same user field values encoded by the runtime.

### V8: State-valued entry parameter

Compile the same logical parameter in two actor contexts.

Expected:

- a current state with both relations identity lowers to SIL `State` directly;
- an expanded current state without route fields does not lower to source-level `State`, despite storage-to-physical identity;
- augmented current state uses a source/user representation and materializes only at a physical boundary;
- source-level call syntax and accepted authored argument shape remain the same;
- aligned `State` substitution may change the generated Sil signature, dispatch tag, script, template hash, and artifact ID;
- field access produces the same result in both contexts.

### V9: Expanded state isolation

Setup: a virtual/expanded source state, first without route fields and then with route fields.

Expected:

- expansion lowering remains a distinct source-to-storage step;
- storage-to-physical identity is allowed when there are no route fields, without falsely claiming source-to-physical identity;
- adding route fields changes only storage-to-physical materialization;
- digest generation, preimage witnesses, and runtime expansion behavior remain unchanged.

### V10: Multi-output exact self

Setup: entry emits the current actor and a peer actor.

Source transition:

```rust
become {
    current <- self,
    peer <- Peer(next_peer),
};
```

Expected:

- exact self applies only to `current`;
- the peer route follows ordinary materialization and template validation;
- bare `become self;` resolves to `current` because it is the only output that permits the active actor;
- swapping the two output roles fails the existing output-shape checks.

### V11: Invalid source-boundary access

Each of the following must fail with an Argent diagnostic:

```rust
AccountState x = self.state;
foo(self);
AccountState x = self;
become foreign <- self; // where foreign cannot be the current actor
```

Expected diagnostics should identify the representation error directly and should not defer it to Silverscript type checking.

### V12: Artifact and runtime compatibility

Build representative tickets, stones, closed ICC, open ICC, actor-enum, spawn, and expanded-state examples before and after the refactor.

Expected:

- user-visible state descriptors are unchanged except for removal of any obsolete `self.state` syntax metadata;
- physical field recipes and hidden witness subjects are unchanged;
- runtime-created script public keys are byte-identical for identical inputs;
- template hashes are unchanged when generated SIL is unchanged;
- dependency artifact IDs and interface fingerprints remain stable unless their specified source inputs intentionally include the changed syntax.

### V13: Global function in two contract contexts

Emit one state-valued global function into an aligned actor and an augmented or expanded actor. The authored function is unchanged, each emitted instance follows its contract's plan, `gen__glob_` isolation remains intact, and unresolved bare identifiers remain errors.

### V14: Actor function authored round trip

Pass an entry state parameter and an authored local through an actor helper, bind the returned state locally, then use it in `become`. Parameter, result, local, and route agree without direct-call recognition or authored-local exceptions.

### V15: Validated peer projection versus whole value

Read `peer.balance` directly from an authenticated physical peer without constructing `PeerState`. In a second call, pass the complete peer to a function expecting `PeerState`; require a real authored value. For expansion, succeed with validated preimages and reject when only the digest is available.

### V16: Nested calls and single evaluation

Compose global and actor state-returning helpers, bind the final result, project multiple fields, and route it. Instrument or inspect generated SIL to prove each effectful state-returning call occurs once.

### V17: State arrays

Cover scalar, fixed-array, and dynamic-array parameters, locals, results, and nested calls in aligned, expanded, and augmented contexts. The array element representation follows the shared state plan. No range-specific conversion is introduced.

### V18: Structured type rewriting

In one Sil function, place the same spelling in parameter/result types, local and destructuring types, array element types, constructors, a variable, a field, a function name, a comment, and a string. Rewrite only genuine type and constructor occurrences, preserve source spans, comments, and formatting, and compile the result through Silverscript.

### V19: Target physical compatibility

Cover two actors that share one source state but have different route cuts, and two different templates whose complete physical layouts are compatible. The first pair must retain separate actor-keyed physical plans despite the shared source type. The second pair may use active `State` on a template-aware read or validation path. Changing the template proof must change builtin authentication only, while changing physical compatibility must change the SIL physical type only.

## 10. Fixture organization

Implement these vectors initially as focused Rust tests and small `.ag`/generated-SIL fixtures beside the relevant compiler layer. Do not build a generic JSON manifest harness as part of this refactor. Introduce one later only if repeated expectations across test layers create enough duplication to justify it.

## 11. Definition of done

The change is complete when:

- `self.state` is absent from the language and `become self` is the exact-continuation form;
- exact self is represented explicitly in the semantic IR and lowers directly to P2SH script-public-key equality;
- every emitted contract owns one typed state-lowering environment with source representations and actor-keyed target physical plans;
- all user-to-physical layout transitions go through the SIL state boundary;
- authored codegen uses Silverscript `State` directly only when nominal source identity and both layout identities hold and the target physical layout is compatible with active `State`;
- global and actor function parameters, results, locals, destructuring, constructors, calls, and state arrays use one per-contract representation decision;
- projected field views are not mistaken for whole authored values, and expanded whole values require validated preimages;
- state-returning calls are evaluated exactly once and in their original order;
- SIL state-type rewriting uses classified AST/type spans rather than textual replacement;
- augmented and expanded cases preserve their current security and runtime behavior;
- physical route fields remain inaccessible from Argent source;
- Rust privacy and typed APIs prevent ordinary codegen from bypassing the physical boundary;
- generated SIL, artifacts, and template hashes have no unexplained diffs;
- all end-state conformance vectors pass under `./check.sh --full`;
- Argent Playground checks and transaction flows pass against the completed compiler.

## 12. Current-source references

- [Silverscript function AST API, PR #222](https://github.com/kaspanet/silverscript/pull/222)
- [Argent global function namespace, PR #49](https://github.com/argent-lang/argent/pull/49)
- [Argent actor functions, PR #50](https://github.com/argent-lang/argent/pull/50)
- [Argent design notes](https://github.com/argent-lang/argent/blob/master/docs/argent-design.md)
- [Compiler layout facts](https://github.com/argent-lang/argent/blob/master/src/compiler/model/layout.rs)
- [Contract and entry emitter](https://github.com/argent-lang/argent/blob/master/src/compiler/codegen/emitter.rs)
- [SIL body and route lowering](https://github.com/argent-lang/argent/blob/master/src/compiler/codegen/sil/body.rs)
- [Generated-code security arguments](https://github.com/argent-lang/argent/blob/master/SECURITY.md)
