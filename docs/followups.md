# Argent follow-ups

This file contains small follow-up items. Each item gives its area, context,
and required work.

## Normalize entry interaction artifacts

**Area:** Portable artifacts and runtime transaction resolution.

**Context:** Entry interactions are represented both in the source-facing
`consumes`/`emits` fields and in `route_plan`. Ranges also make exact covenant
and authorized-output indices optional when a preceding range has a runtime
length.

**Follow-up:** Keep cardinality and structural locations on one canonical
interaction record. Remove duplicate consume and output records from
`route_plan`, retaining only route-specific relationships and witness data.

## Isolate state-boundary code generation

**Area:** State-boundary code generation.

**Context:** `state_boundary.rs` still imports the complete emitter module.
Layout planning and emission also calculate some generated names separately.
Generated output fields lose their typed provenance when they become a map of
field IDs to Sil expressions.

**Follow-up:** Give the state boundary explicit naming, packing, witness, and
rendering inputs instead of importing `emitter::*`. Centralize generated actor
template and route-family field names. Keep each generated field source typed
until final Sil rendering instead of reducing it to a string expression.

## Define conversion from physical `State`

When both state-layout relations are identity, `CounterState` and physical
`State` have exactly the same fields. This currently compiles:

```rust
State raw = readInputState(index);
CounterState value = raw;
```

Using `raw` directly as the successor state is rejected:

```rust
become next <- Counter(raw);
```

Decide whether the typed assignment is an intentional conversion or whether
both forms should follow the same rule. Direct `readInputState` use remains
low-level and developer-managed; it does not authenticate the input.

## Infer the leader of a singleton-app delegate

**Area:** Delegate modeling, covenant-input validation, and artifacts.

**Context:** A delegate currently names its leader as the first `consumes`
actor. This lets the generated prelude authenticate the leader input's
template. In an app with only one actor, the only in-app leader template is
already determined by the delegate actor, so requiring a source-level leader
handle solely for that proof is redundant.

**Follow-up:** Allow a delegate in a singleton-actor app to omit the leader
from `consumes`. Infer the same actor as its leader and keep the generated
template authentication of the concrete leader input. Require an explicit
handle when the delegate body needs to inspect the leader's state or value.
Record the inferred leader relationship explicitly in the artifact.

## Allow open observed groups

**Area:** Observe syntax, transaction-shape validation, artifacts, and runtime
resolution.

**Context:** An `observes` clause currently describes the complete observed
input and output groups. An observer may need to authenticate and follow a
known subset while permitting unrelated inputs or outputs in the same
transaction.

**Follow-up:** Add an explicit open-group marker to observed `inputs` and
`outputs`. Declared handles remain authenticated and available to the body,
while the generated checks permit additional group members.

Define where extra members may occur. Restricting them to a declared rest
position keeps handle indices derivable; allowing them anywhere requires
explicit indices or an unambiguous matching rule. Record openness and the
binding rule in the artifact so the runtime and transaction builder resolve
the same handles.

## Authenticate actors with no runtime state

**Area:** Actor-type handles and compiler code generation.

**Context:** `readInputStateWithTemplate` authenticates a template while it
decodes state fields. It is not the right operation when an actor's complete
physical state, including compiler-owned fields, is empty. In that case the
redeem script is fixed, so its P2SH hash identifies the complete actor script.

Silverscript can construct the locking script from a runtime hash with
`new ScriptPubKeyP2SH(hash)`.

**Follow-up:** Represent an empty-state actor-type handle by its redeem-script
hash. Authenticate its inputs and outputs by comparing their script public keys
instead of generating an empty state read or validation.

The default handle for such an actor must be `blake2b(redeem_script)`, with the
complete script hashed as one unit. It is not Silverscript's template hash over
the length-delimited template prefix and suffix. With no state boundary to
preserve, the exact redeem script is the actor identity.

For a static actor reference, embed the expected script public key as a contract
constant. For a runtime actor-type handle, construct it with
`new ScriptPubKeyP2SH(handle)`. Keep using the typed state-template operations
when the effective physical state is non-empty, even if the authored state has
no fields.

Add pinned generated-Sil and runtime coverage for static and runtime-selected
empty-state actors, including rejection of an incorrect actor-type handle.

## Decode observed application transactions

**Area:** `argent-rt`, the artifact ABI, and application observers or indexers.

**Context:** An observer can use covenant IDs to find raw Kaspa transactions
that belong to an application. It must decode the input actor state, the entry
call, and the output actor states. It uses these values to reconstruct the
application state.

The Argent artifact describes both user-declared ABI values and generated ABI
values.

Application code must currently know generated field or argument names such as
`gen__mux_routes`. These names are compiler details. An actor rename can change
them and break an observer.

**Follow-up:** Add `argent-rt` helpers that use the artifact to decode
application transactions. Return user-declared values with their source names.
Provide stable accessors for generated templates and route data.

Validate the actor, entry, state layout, and value types against the artifact.
Add a test in which an actor rename changes generated ABI names. The observer
must continue to work without a code change.

## Launch proofs

**Area:** `argent-rt`, launch APIs, and audit tools.

**Context:** One genesis output group launches one covenant. The covenant ID
depends on the authorizing funding outpoint and the ordered outputs. One
transaction can launch more than one covenant.

An auditor can find a live covenant UTXO without knowing its launch transaction
or initial actor states.

Argent has no standard proof package that explains how a live covenant ID was
launched. An auditor must collect and check the launch data manually.

**Follow-up:** Support one launch proof for each genesis covenant group. A
transaction that launches multiple covenants can have multiple proofs.

The proof must contain:

- The authorizing funding outpoint.
- The covenant ID that the system calculates from the outpoint and the ordered
  launch outputs.
- Each initial actor state.
- The redeem-script preimage for each output. This preimage contains the
  template prefix, the encoded state, and the template suffix.
- The related P2SH script public key for each output.

Verification must prove:

- The actor state encodes to the specified redeem script.
- The redeem script hashes to the launch-output script public key.
- The ordered outputs and the authorizing outpoint produce the specified
  covenant ID.

This proof lets an auditor confirm which contracts and initial states started
the live covenant.

## KCC20 bootstrap with `spawns`

**Area:** ICC examples, ICC documentation, and `argent-rt` runtime tests.

**Context:** The Argent KCC20 example has a `Minter` controller app and a
KCC20 asset app. During mint, `Minter` observes the asset-side `MinterProxy`.
The controller state stores the asset covenant ID.

The `kcc20_covenant_minter` test in Silverscript uses two transactions:

1. Launch an uninitialized `Minter` covenant.
2. Create the asset covenant and initialize `Minter` in the same transaction.

The Argent example implements mint but does not implement this bootstrap
sequence. It does not show how the controller receives the asset covenant ID.

**Follow-up:** Add `Minter::init`. Use `spawns` to create the asset covenant. Do
not add a separate genesis proof. The `spawns` lowering already proves that the
active `Minter` input creates the declared covenant group.

Use an `actor_type<MinterProxyState>` value for the proxy. Store it in the
uninitialized controller state. The controller needs this value because
`MinterProxy` belongs to a different app.

The entry has this shape:

```rust
entry init(sig owner_sig)
spawns asset by asset_id {
    outputs {
        proxy: self.proxy_type,
    }
}
emits next: Minter {
    require(!initialized);
    require(checkSig(owner_sig, owner));

    MinterProxyState proxy_state = {
        controller_id: self.cov_id,
    };
    unrestricted(asset.outputs.proxy.value);
    require asset.outputs become {
        proxy <- self.proxy_type(proxy_state),
    };

    MinterState next_controller = {
        owner: owner,
        proxy_type: proxy_type,
        kcc20_covid: asset_id,
        amount: amount,
        initialized: true,
    };
    unrestricted(next.value);
    become next <- Minter(next_controller);
}
```

Use the `spawn::asset` path in `argent-rt`. Add a runtime test for bootstrap
followed by mint. Add the source example to the ICC documentation. This work
does not need new spawn lowering.

## Correlated output variants

**Area:** Language syntax, compiler route analysis, artifacts, and typed
builders.

**Context:** An `emits` declaration can give each output a union of possible
actors. The compiler treats each output union independently.

For example:

```rust
emits {
    left: A | C,
    right: B | D,
}
```

This declaration permits `(A, B)`, `(A, D)`, `(C, B)`, and `(C, D)`.

An application can require only `(A, B)` or `(C, D)`. The current syntax cannot
declare this relationship. The entry body must reject `(A, D)` and `(C, B)`.
The artifact also cannot describe the relationship to a typed builder.

**Follow-up:** Let `emits` declare valid output groups:

```rust
emits {
    left: A,
    right: B,
} | {
    left: C,
    right: D,
}
```

This form permits only `(A, B)` and `(C, D)`.

At first, all alternatives must use the same output names and order. The
compiler must verify that each terminal route uses one declared alternative.
The artifact must record the source alternatives. A typed builder can then
match the outputs without exposing a terminal path index.
