# State boundary review checklist

This is a temporary review document for the `actor_functions` branch. It
combines the local end-to-end review with the architecture review. Remove it
before merge after every remaining item is fixed, accepted, or moved to a
long-term note.

Reviewed code:

- branch head: `2e33aab`
- base: `ddde966` (`master`)
- architecture review: through `bacb308`

The order below is the suggested fix order. It fixes shared state conversion
operations before their remaining consumers. Each correctness fix should
start with a focused regression test when practical.

## Required before merge

### 1. [x] Use one source-to-storage digest operation

**Resolved:** Input references, named authored values, and output expansion
hashing now use one `SourceStorageRelation`-driven operation. The active input
reuses an authenticated stored expansion digest after checking that its
validated opening can reconstruct the complete authored value. Named authored
values derive the same digest from their nested fields.

Input-reference and named-value digests use different implementations. For an
expanded state:

```rust
Expanded snapshot = state(self);
byte[32] direct = digest(state(self));
byte[32] local = digest(snapshot);
```

The direct form included the expansion digest. The named form previously emitted:

```rust
byte[32] local = blake3(byte[](0x));
```

Expanded source declarations have no ordinary fields, so the legacy field
packer hashed an empty payload.

The shared encoder handles expansion digests, explicit empty payloads, and
excludes generated route fields. Broader typed expression support remains in
item 6.

Completion criteria:

- the two digests above are identical;
- expansion-backed fields use their validated storage digests;
- empty states still use explicit empty bytes;
- generated route fields never enter the digest;
- a nontrivial state expression is evaluated once;
- either support `digest(snapshot())` and other proven authored expressions,
  or explicitly document the narrower named-value rule.

### 2. [x] Make state-constructor parsing fail closed

**Resolved:** Every nontrivia top-level constructor component must be a valid
`name: expression` field or compilation fails with an Argent diagnostic.

The handwritten field splitter previously dropped any top-level component
without a colon:

```rust
CounterState candidate = CounterState {
    count: 1,
    validation_call(),
};
```

This compiled, but `validation_call()` did not appear in generated Sil.

The existing focused parser now proves that it consumed every component.
Malformed or unclassified components are never discarded.

Also preserve valid comments and trivia. For example, this should not depend
on the textual `split_state_constructor` heuristic:

```rust
become next <- Counter(CounterState /* authored */ {
    count: count + 1,
});
```

### 3. [x] Lower bare expanded current fields consistently

**Resolved:** Bare active fields use the same authenticated projection as
`self.<field>`. This works in locals, helper arguments, arrays, and successors,
and follows lexical shadowing. Direct whole-value destructuring is rejected
with guidance to project the required fields individually.

The active binding table types an expanded field such as `detail` as authored
`Details`. The opening-backed replacement supports `self.detail` and qualified
field access, but not the bare actor-field form:

```rust
Details copy = detail;
```

This reaches Sil as the physical `byte[32] detail` field and fails with a type
error. It does not currently compile as silent wrong code.

Either lower bare `detail` through the active input reference or reject it with
a clear Argent diagnostic. Apply the same rule in helper arguments, locals,
destructuring, arrays, and successors.

### 4. [x] Restore linked authored state declarations

**Resolved:** `ContractStateValuePlan` now records the source representations
required by local states, constants, callable signatures, entry parameters,
entry-body declarations, and resolved input/output targets. It closes their
nested state dependencies. State layout emission consumes that plan and no
longer discovers authored types from observe or spawn clause syntax.

A linked state used only in an entry signature is no longer declared in the
generated contract:

```rust
import app ChildApp from "./child.ag";

actor Local owns LocalState {
    entry hold(ChildState value) emits none {
        require(value.amount >= 0);
    }
}
```

`master` emits `struct ChildState` and compiles. This branch omits the struct
and fails during Sil compilation.

Drive authored struct emission from the named source representations in
`ContractStateValuePlan`, instead of rediscovering types from selected clause
syntax. Cover linked states used only by scalar, fixed-array, and dynamic-array
entry parameters, functions, constants, and body locals.

### 5. [x] Reject physical `State` in external entry parameters

**Resolved:** Entry-signature validation rejects scalar, fixed-array, and
dynamic-array physical `State` parameters with an Argent-authored-state
diagnostic. This rule does not depend on whether the active contract layout is
equivalent to `State`. Physical `State` remains valid in locals and global or
actor helper signatures.

This source previously compiled:

```rust
entry inspect(State supplied) emits none {
    require(1 == 1);
}
```

For an augmented actor, the Sil ABI resolves this parameter against the full
runtime `State`. The runtime caller must supply compiler-owned route fields.
Successor proof rejects the value, so no route-field bypass is currently
known, but the external ABI contradicts the compiler boundary.

Reject `State` and `State[]` in entry parameters. Keep intentional physical
`State` locals and global or actor helper parameters/results.

### 6. [x] Complete proven authored-expression coverage

**Resolved:** The state-value plan now carries typed shared constants and
callable results into route and digest lowering. Lexical bindings take
precedence over same-named constants. A generated typed digest helper evaluates
a non-identifier value once before it projects and packs the state fields.

The proven expression surface now includes:

- typed local bindings and shared constants;
- direct global and actor function results with a planned signature;
- constructors at a typed route, call, local, or array boundary;
- typed array literals, `append(...)`, and indexing of planned arrays.

This is not a general expression typechecker. A linked constructor used only
inside an otherwise untyped expression remains deferred. Supporting that case
requires feeding AST-derived expression uses back into contract declaration
planning before state layouts are emitted. The compiler does not use a textual
type guess for this case.

Before this fix, some typed authored values required an unnecessary local
binding:

```rust
become next <- Counter(INITIAL); // typed CounterState constant
```

The route rejected `INITIAL` until it was first bound to a `CounterState` local.
Likewise, `digest(snapshot())` rejected a direct call even when `snapshot()` had
a proven state result.

These paths use the shared state-value and source-to-storage plans. They do not
add separate syntax-specific type tests.

## Decisions and documentation

### 7. [x] Define `SourceStateId` identity precisely

**Decision:** State identity follows Argent's unqualified state namespace.

Same-name, definition-equivalent local or linked states are aliases of one
canonical source state within a compiled model. Conflicting declarations are
rejected. Different names remain distinct even when their layouts are equal.
The rule is recorded in `compiler-design.md` and on `SourceStateId`.

### 9. [x] Correct small documentation drift

- The template hash in `argent-design.md` uses Blake3.
- `state_boundary.rs` describes its input and output responsibilities.
- `compiler-design.md` states the current `digest(...)` expression surface.

## Non-blocking architectural follow-ups

### 10. [ ] Remove the state-boundary dependency on `emitter::*`

`state_boundary.rs` imports the complete emitter module. Extract the naming,
packing, witness, and rendering inputs that the boundary actually needs. This
can remain a follow-up if the correctness fixes do not expose the natural
interface.

### 11. [ ] Centralize generated symbol naming

Actor-template and route-family field names are calculated independently in
`model/layout.rs` and `codegen/emitter.rs`. Move each algorithm to one shared
naming API so layout planning and declaration emission cannot diverge.

### 12. [ ] Type generated-field sources

`OutputStateTarget` reduces trusted generated-field provenance to
`BTreeMap<GeneratedFieldId, String>`. A later `GeneratedFieldSource` enum could
keep the provenance typed until final Sil rendering.

### 13. [ ] Split the remaining backend concentration

`emitter.rs` and `body.rs` still combine several responsibilities. Defer the
split until the correctness work reveals stable interfaces. Likely boundaries
include authored-value proof, state conversion, naming, and rendering.

## Validation for each correctness commit

Run:

- the focused regression test first;
- `./check.sh --full` in Argent;
- VS Code tests when syntax or diagnostics change;
- Argent Playground `./check.sh` when generated contracts or source syntax
  change;
- regeneration and semantic review of every generated Sil or artifact diff;
- `git diff --check` for staged and unstaged changes.
