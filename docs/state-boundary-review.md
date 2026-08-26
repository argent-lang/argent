# State boundary review checklist

This is a temporary review document for the `actor_functions` branch. It
combines the local end-to-end review with the architecture review. Remove it
before merge after every remaining item is fixed, accepted, or moved to a
long-term note.

Reviewed code:

- branch head: `2e33aab`
- base: `ddde966` (`master`)
- architecture review: through `bacb308`

The order below is the suggested fix order. It puts semantic proof before the
operations that consume that proof. Each correctness fix should start with a
focused regression test when practical.

## Required before merge

### 1. [ ] Require positive authored-value proof

**Status:** Confirmed locally. Blocker.

An expression can enter an authored position when Argent cannot prove its
origin. Equivalent-`State` lowering then erases the difference between an
authored source value and a physical value.

Both examples compile:

```rust
CounterState forged = readInputState(attacker_index);
become next <- Counter(forged);
```

```rust
fn identity(CounterState value) -> CounterState {
    return value;
}

State physical = readInputState(attacker_index);
become next <- Counter(identity(physical));
```

The generated Sil passes the physical value to `validateOutputState`. The
direct physical-successor check does not protect assignment or function-call
laundering.

Replace the current “not proven incompatible” rule with positive provenance.
Every authored initializer, assignment, function argument, array element,
function result, and successor must carry a proven source identity and shape.
Unknown and physical values must fail closed. Function bodies must prove their
returns before equivalent-`State` rewriting changes the Sil spelling.

Completion criteria:

- reject both examples above;
- preserve valid authored locals, calls, arrays, and successors;
- prove shared state constants before they are treated as authored values;
- keep physical `State` locals and helper signatures usable only in physical
  positions.

### 2. [ ] Use one source-to-storage digest operation

**Status:** Confirmed locally. Blocker; silent wrong code.

Input-reference and named-value digests use different implementations. For an
expanded state:

```rust
Expanded snapshot = state(self);
byte[32] direct = digest(state(self));
byte[32] local = digest(snapshot);
```

The direct form includes the expansion digest. The named form currently emits:

```rust
byte[32] local = blake3(byte[](0x));
```

Expanded source declarations have no ordinary fields, so the legacy field
packer hashes an empty payload.

There must be one authored-to-storage payload encoder driven by
`SourceStorageRelation`. Input references, locals, parameters, constants,
function results, output expansion, and `digest(...)` must use it.

Completion criteria:

- the two digests above are identical;
- expansion-backed fields use their validated storage digests;
- empty states still use explicit empty bytes;
- generated route fields never enter the digest;
- a nontrivial state expression is evaluated once;
- either support `digest(snapshot())` and other proven authored expressions,
  or explicitly document the narrower named-value rule.

### 3. [ ] Make state-constructor parsing fail closed

**Status:** Confirmed locally. Blocker; source code is silently removed.

The handwritten field splitter drops any top-level component without a colon:

```rust
CounterState candidate = CounterState {
    count: 1,
    validation_call(),
};
```

This compiles, but `validation_call()` does not appear in generated Sil.

State constructors must use structured Sil AST fields, or another parser that
proves it consumed the complete constructor. Every malformed or unclassified
component must produce an Argent diagnostic.

Also preserve valid comments and trivia. For example, this should not depend
on the textual `split_state_constructor` heuristic:

```rust
become next <- Counter(CounterState /* authored */ {
    count: count + 1,
});
```

### 4. [ ] Lower bare expanded current fields consistently

**Status:** Confirmed locally. High.

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

### 5. [ ] Restore linked authored state declarations

**Status:** Confirmed locally. Regression from `master`.

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

### 6. [ ] Reject physical `State` in external entry parameters

**Status:** Confirmed locally. Pre-existing boundary gap.

This source compiles:

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

### 7. [ ] Complete authored-expression coverage

**Status:** Confirmed locally. This is a conformance pass after items 1–3.

Some typed authored values still require an unnecessary local binding:

```rust
become next <- Counter(INITIAL); // typed CounterState constant: rejected
```

Binding `INITIAL` to a `CounterState` local first makes the route compile.
Likewise, `digest(snapshot())` is rejected even when `snapshot()` has a proven
state result.

Use the positive provenance from item 1 in route, digest, call, array, and
constructor lowering. Do not create separate syntax-specific type tests.

## Decisions and documentation

### 8. [ ] Define `SourceStateId` identity precisely

**Status:** Architecture ambiguity; no incorrect generated code is confirmed.

`SourceStateId` contains only the state name. Linked states with the same name
and definition are coalesced. This is consistent with Argent's current
unqualified state namespace, but the type documentation calls the ID the
identity of one declaration.

Choose and document one rule:

- same-name, same-definition linked states are aliases of one canonical source
  state within a compiled model; or
- source IDs include application or declaration provenance.

The alias rule matches the current language and is the smaller clarification.

### 9. [ ] Decide the pre-release artifact-version policy

**Status:** Policy decision, not a confirmed compiler bug.

`RouteArtifact` changed from flat constructed-route fields to the tagged
`RouteSuccessorArtifact`, while the schema version remains 1. Old and new
development artifacts are not mutually readable.

The project has previously kept version 1 across pre-release breaking changes.
Either keep that policy explicitly, or bump the schema to 2 because the
artifact verifier treats the version as a compatibility contract.

### 10. [ ] Correct small documentation drift

- `argent-design.md` still shows the template hash as Blake2b; the compiler
  uses Blake3.
- `state_boundary.rs` describes only authenticated inputs, although it also
  owns output materialization and proof selection.
- After the fixes, update `compiler-design.md` so its provenance and digest
  statements match the exact supported expression surface.

## Non-blocking architectural follow-ups

### 11. [ ] Remove the state-boundary dependency on `emitter::*`

`state_boundary.rs` imports the complete emitter module. Extract the naming,
packing, witness, and rendering inputs that the boundary actually needs. This
can remain a follow-up if the correctness fixes do not expose the natural
interface.

### 12. [ ] Centralize generated symbol naming

Actor-template and route-family field names are calculated independently in
`model/layout.rs` and `codegen/emitter.rs`. Move each algorithm to one shared
naming API so layout planning and declaration emission cannot diverge.

### 13. [ ] Type generated-field sources

`OutputStateTarget` reduces trusted generated-field provenance to
`BTreeMap<GeneratedFieldId, String>`. A later `GeneratedFieldSource` enum could
keep the provenance typed until final Sil rendering.

### 14. [ ] Split the remaining backend concentration

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
