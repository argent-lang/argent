# Argent compiler design

This document records the current internal architecture of the Argent
compiler. It describes established compiler boundaries and invariants. It will
grow as other compiler areas become stable enough to document.

## State layout and lowering

Argent separates user-authored state from the state that a generated contract
stores and validates. This boundary prevents compiler-owned route data from
entering the source type system.

### State representations

The compiler uses three state representations.

#### Source state

Source state is the state that Argent code can use. It has a nominal state
identity and contains only fields that the user declared.

For example:

```rust
state AccountState {
    byte[32] owner;
    int balance;
}
```

`AccountState` is source state. Argent code can construct it, pass it to a
function, store it in a local or array, and use it to construct a successor.

Source state never contains route templates, route-family digests, template
witnesses, or `gen__*` fields.

#### Storage payload

The storage payload is the on-chain form of the user state before the compiler
adds route fields.

For an ordinary state, the source state and storage payload have the same
layout. For an expanded state, a source field can contain a structured value
while the storage field contains its digest. Expansion lowering performs this
source-to-storage conversion.

The storage payload contains no route fields.

#### Physical state

Physical state is the complete state layout that a generated Silverscript
contract stores or passes to a state builtin.

```text
physical state = compiler-owned route context + storage payload
```

The active contract uses Silverscript's `State` type for this layout. A foreign
physical layout can use `State` when it is compatible with the active layout.
Otherwise, Argent emits a named physical struct.

The generated layout can stay flat. The three representations are semantic
categories. They do not require a nested wire encoding.

### Layout relations

The compiler plans two independent relations:

```text
source state  ──expansion lowering──>  storage payload
storage      ────route lowering────>  physical state
```

Each relation is an identity or a transformation.

| Case | Source to storage | Storage to physical |
| --- | --- | --- |
| Ordinary state, no route fields | Identity | Identity |
| Expanded state, no route fields | Transformation | Identity |
| Ordinary state with route fields | Identity | Augmented |
| Expanded state with route fields | Transformation | Augmented |

An identity between storage and physical state does not make physical `State`
an authored value. An expanded state without route fields is the main example.
Its physical value contains expansion digests, not the structured source
fields.

### Per-contract layout plan

The model builds one immutable `ContractStateLowering` plan for each emitted
actor contract. The plan contains:

- the active source, storage, and physical layouts;
- the source representation selected for each nominal state type;
- the physical plan for each actor, open state, or selector target;
- stable mappings from source fields to storage and physical fields;
- the physical Silverscript type selected for each output target.

Target plans are actor-context keyed. Two actors can own the same source state
and still require different route fields. Two templates can use the same
physical `State` type when their complete physical layouts are compatible.

Physical compatibility is semantic. The compiler compares field order, field
identity, Silverscript type, width, and generated-field role. Equal byte widths
are not sufficient.

The compiler makes two separate type decisions:

```text
Can authored state use State?
    The nominal source is the active owned state.
    Source-to-storage is identity.
    Storage-to-physical is identity.

Can a physical target use State?
    The target physical layout is compatible with active State.
```

A route-bearing source state therefore remains a named authored struct even
when its target physical value can use `State`.

### Authenticated input boundary

Each entry has one typed input-reference plan. It covers:

- the active `self` reference;
- each consumed input handle;
- each observed input reference.

Each reference records its nominal source identity, physical type, input-index
expression, and available source-field projections. An external reference also
records its template proof. Active `self` needs no planned template proof
because it is the executing input. Physical expressions remain private to the
boundary.

An input reference supports two forms of access:

```text
ref.field       project one authenticated source field
state(ref)      reconstruct the complete authored source value
```

A projection does not construct fields that the expression does not use.
Complete reconstruction excludes compiler-owned fields and preserves the
nominal `SourceStateId`.

`SourceStateId` identifies one unqualified state name within a compiled model.
Equivalent local or linked declarations with the same name are aliases and
share this identity. Conflicting same-name declarations are rejected. Different
state names remain distinct even when their layouts are equal. Actor identity
remains application-qualified because actors can have different templates and
route context while owning the same source state.

An expanded source field requires a validated opening. A direct projection
requires only the opening for that field. Complete reconstruction requires
openings for all expanded fields. The compiler rejects reconstruction when the
entry plan has only storage digests.

Input authentication and source projection are separate operations. For an
external reference, the compiler first proves the physical input template.
Active `self` is already the executing input. The compiler then exposes only
the source fields authorized by the layout plan.

The source-language rules for input references are in
[Argent design notes](argent-design.md#input-references-and-authored-state).

### Authored values in functions and arrays

The compiler selects one authored representation for each source state in each
emitted contract. Global functions use the plan of the contract into which the
compiler emits them. Actor functions use the plan of their owner contract.

The same plan applies to:

- function parameters and results;
- entry parameters;
- local and destructuring bindings;
- state constructors;
- scalar, fixed-array, and dynamic-array elements;
- function-call arguments and results.

The call site and callee therefore use the same representation. The decision
does not depend on the syntax of an initializer or on whether a value goes
directly to a successor.

A state-producing expression is evaluated once. Lowering can then project its
fields, compute expansion digests, or materialize route fields. It must not
repeat the original expression or move it across a conditional boundary.

When an authored type is equivalent to active `State`, Argent uses the
Silverscript AST to change only type and constructor occurrences. It applies
checked edits at classified spans and reparses the result. It does not replace
matching text in variables, fields, functions, comments, or strings.

### Storage payload digest

`digest(authored_state)` computes the Blake3 digest of an authored state's
storage payload. The compiler first applies the source-to-storage relation. It
then packs the storage fields in their declared ABI order and hashes the packed
bytes.

Expansion-backed fields contribute their validated storage digests. Generated
route fields do not contribute. The current entry-body surface accepts
`state(ref)` and named authored bindings. Bind another typed authored
expression to a local before passing it to `digest(...)`.

### Successor materialization

A constructed successor has an authored source value and a resolved physical
target. The output boundary converts the value in two steps:

```text
authored source value
        │
        │ expansion lowering
        ▼
storage payload
        │
        │ add compiler-planned route context
        ▼
complete physical successor
```

Every physical field has one planned origin.

- A user field comes from the authored value through its source-to-storage
  relation.
- A generated field comes from compiler context, an authenticated input, a
  linked template, or a planned route transition.

User code cannot provide a generated field. Physical `State` values cannot be
used as authored successor values.

The boundary stabilizes a nontrivial authored expression before it projects
multiple fields. This preserves single-evaluation and source-order semantics.

### Exact continuation

`output <- self` is a resolved `ExactSelf` successor. It does not construct a
source, storage, or physical state value.

The generated contract compares the output script public key with the active
input script public key. This preserves the complete covenant instance state,
including generated fields. Exact continuation does not use an output-state
validation builtin.

### Template proof

Physical materialization and template authentication are independent.

| Available template proof | Output builtin |
| --- | --- |
| Current template | `validateOutputState` |
| Authenticated bound input | `validateOutputStateWithInputTemplate` |
| Explicit template witness | `validateOutputStateWithTemplate` |

Each builtin receives a complete physical state value. The target layout plan
selects its physical type. The proof plan selects the builtin and its template
arguments. Neither decision implies the other.

### Compiler ownership

The implementation divides this work across these modules:

- `src/compiler/model/layout.rs` builds typed source, storage, physical, and
  per-contract layout plans.
- `src/compiler/codegen/sil/state_boundary.rs` owns authenticated input
  projection, authored reconstruction, output materialization, template-proof
  selection, and exact preservation.
- `src/compiler/codegen/sil/state_values.rs` plans state-valued signatures,
  bindings, and array shapes.
- `src/compiler/codegen/sil/state_types.rs` applies checked Silverscript AST
  edits to authored type and constructor positions.
- `src/compiler/codegen/sil/body.rs` preserves source control-flow order and
  requests typed boundary operations.
- `src/compiler/codegen/emitter.rs` emits contract declarations, entries, and
  artifacts from the completed plans.

Physical layout knowledge is limited to layout planning, the state boundary,
contract declaration emission, artifact emission, and runtime physical-state
construction. Ordinary expression and function lowering cannot construct a
trusted physical value directly.

### Required invariants

1. User state contains no compiler-owned route field.
2. Every source, storage, or physical conversion uses a typed boundary
   operation.
3. The compiler plans layouts once per emitted contract.
4. Field mappings use stable semantic identities, not string positions.
5. Input template proof occurs before physical input state is trusted.
6. Complete input reconstruction preserves nominal source identity.
7. Expanded reconstruction requires validated preimages.
8. Generated successor fields come only from compiler-planned context.
9. Template proof does not select the physical layout.
10. Physical layout does not select the template proof.
11. Exact continuation is an explicit semantic route.
12. State-producing expressions keep single-evaluation and source order.
13. The artifact records the physical layout and each generated field role.
14. Generated Silverscript has no unexplained layout or identity change.

### Coverage

Tests cover the four combinations of expansion and route augmentation. They
also cover:

- active, consumed, observed, open, linked, and selector targets;
- direct field projection and complete `state(ref)` reconstruction;
- valid and unavailable expansion openings;
- global and actor function parameters, results, locals, and nested calls;
- scalar, fixed-array, and dynamic-array state values;
- aligned `State` selection and named authored layouts;
- current, bound-input, and witnessed template proofs;
- exact continuation and mixed successor blocks;
- adversarial changes to user fields, generated fields, templates, and
  witnesses;
- generated Silverscript, artifact, runtime, and transaction behavior.

The main focused fixtures are under `tests/fixtures/state_layout/`.
`./check.sh --full` regenerates the tracked Argent examples. The Argent
Playground check runs the transaction flows.
