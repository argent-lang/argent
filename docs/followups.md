# Argent follow-ups

This file contains small follow-up items. Each item gives its area, context,
and required work.

## Require an output-value disposition

**Area:** Entry validation and body analysis.

**Context:** An entry can validate an emitted actor's state and template while
placing no restriction on the output's Kaspa value. Omitting a value policy is
easy to miss during review.

**Follow-up:** Require every emitted output handle to account explicitly for
its `.value` on every terminal path. A value can be constrained by an enforced
boolean expression or deliberately accepted with:

```rust
unrestricted(next.value);
```

Define precisely which boolean uses count. Merely computing and discarding a
comparison must not satisfy the rule. Start with direct use in `require(...)`
and conditions that govern a terminal path; decide separately how annotated
helper functions can preserve the guarantee. Report the output handle and
uncovered terminal path in diagnostics.

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

## Resolve observed state read types

**Area:** Compiler code generation and Silverscript type checking.

**Context:** Argent assigns each `readInputStateWithTemplate` result to the
declared state type. Two valid observation shapes do not compile:

- An observed actor has an empty state.
- Two observed actors have different state names but the same Sil field
  layout.

In the second case, Silverscript reports more than one matching struct layout
even though the generated assignment names the target type. These failures also
occur for actors in one app. They are not specific to app linking.

**Follow-up:** Define how an empty-state observation authenticates the input
template without a state value to decode. Make Silverscript use the declared
assignment type before it tries to match a struct by field layout.

Add compiler tests for both shapes. Add a runtime test that proves the observed
input template is still authenticated.

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
