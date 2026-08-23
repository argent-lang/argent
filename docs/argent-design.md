# Argent design notes

Argent is an actor-style frontend for building multi-contract Silverscript apps
as well-formed covenant state machines. This document records the current
language and compiler design.

The focus is the compiler/runtime boundary: generated Silverscript, portable
artifacts, ICC flows, route families, and the source features needed to make
those pieces usable from Argent.

## Core design

- Argent emits plain Silverscript, not Silverscript covenant macros.
- User state is declared once with `state`.
- `actor` owns persistent covenant state.
- `entry` declares callable transition paths.
- `emits` declares authorized output shape.
- `become` is tail-dispatch into successor actor state.
- Typed covenant inputs hide `readInputStateWithTemplate` boilerplate.
- Basic Silverscript data types stay visible as-is, such as `temporal`, `sig`, and `pubkey`;
  Argent does not invent wrapper primitive types for them.
- Prefix/suffix witnesses are generated Silverscript ABI, not Argent source
  surface.
- Every `become` route must be allowed by the entry's `emits` declaration.
- Covenant input/output shape is a first-class Argent concern.
- The main language/compiler contribution is hiding route logic, template
  propagation, and mechanical safety checks from application code.
- Named actor flows use a leader-auth output pattern by default.
- True cov-output N:M transitions are singleton per covenant id per transaction;
  ordinary named actor flows use auth-output coordination instead.
- Helper function bodies are expected to already be valid Silverscript-shaped
  code. Silverscript remains responsible for final helper/body validity where
  Argent has not lowered the expression itself.

## Application and covenant domains

An Argent app is a compile-time actor domain. A covenant ID identifies one
runtime covenant instance.

The model uses these rules:

- One covenant ID belongs to one app. Actors that share a covenant ID cannot
  come from different apps.
- One app can have many covenant IDs. Each launch subset or spawned actor group
  can start a separate instance of the app.
- The same source actor can be a member of several apps. Each `App::Actor` is a
  separate compile target. It has the route context and actor handle of its
  app.
- `consumes` and `emits` stay inside one app. They are part of the selected
  app's route graph and commitment cuts.
- `observes` connects the current covenant to an existing covenant. The target
  can be in the same app or in a different app.
- `spawns` connects the current covenant to a new covenant. It has the output
  semantics of `observes` with no inputs. It also requires a genesis check.
- App dependencies form a directed acyclic graph. A source bundle compiles each
  app once, after its dependencies.

An importing app does not compile a foreign `App::Actor` in its own context.
It reads the actor interface and actor handle from the foreign app artifact.

## Surface syntax conventions

Argent uses different word orders for declarations and bindings. The syntax
reflects the purpose of each construct.

Declarations and value bindings follow Silverscript syntax. Argent-specific
transaction clauses extend the binding model to transaction roles and routes.

### Declarations

State fields, local variables, and callable parameters are declarations. A
declaration puts the type before the name.

```rust
state WalletState {
    int credits;
    pubkey initializer;
}

entry transfer(pubkey pk, int amount)
```

Type-first declarations preserve a direct high-level-to-Sil surface.

### Value bindings

A state value expression binds each field name to a value expression. A colon
separates the field name from the value expression. Commas separate the
bindings.

```rust
AgentCapsule next_agent = {
    energy: prev_state.energy - 1,
    generation: prev_state.generation,
};
```

`AgentCapsule next_agent` is a type-first declaration. The items in the braces
are value bindings. The final semicolon terminates the declaration.

### Role bindings

The `consumes`, `emits`, `spawns`, and `observes` clauses bind role names to
actor targets. A colon separates the role name from the actor target. Commas
separate the role bindings. Output handles are always named; a singleton can
use the unbraced `emits name: Actor` shorthand.

An actor target can be a fixed actor or a dynamic actor handle. This `observes`
clause uses a dynamic actor handle:

```rust
observes remote by self.agent_cov_id {
    inputs {
        agent: self.agent_type,
    }

    outputs {
        agent: self.agent_type,
    }
}
```

`agent` is the role name. `self.agent_type` selects the actor
implementation at runtime.

### Route bindings

A `become` block binds each output role to its next actor and state. The `<-`
operator separates the output role from the successor expression. Commas
separate the route bindings.

```rust
become {
    white_out <- Player(next_white),
    black_out <- Player(next_black),
};
```

`white_out` and `black_out` refer to roles in the `emits` clause. The final
semicolon terminates the `become` statement.

A singleton route can omit the braces:

```rust
become next <- Player(next_player);
```

### Consistency rule

Declarations put the type before the declared name. Value, role, and route
bindings put the local name on the left. Commas separate items in binding
lists. Semicolons terminate declarations and statements.

## Execution context ladder

Silverscript provides `tx` and `this`. Argent adds `self`. Together, these names
form an abstraction ladder:

```text
tx      complete transaction
this    active consensus input and script
self    logical Argent actor
```

For example, `tx.outputs[i].value` reads the transaction, and
`this.activeInputIndex` identifies the input that runs the script.

The ladder moves through three abstraction levels. `tx` exposes the complete
transaction. `this` identifies the active input and script. `self` presents the
active input as a logical Argent actor.

`self` is a context namespace. It is not an actor handle or another first-class
actor value. Its valid and reserved members are:

```text
self.value  // Native KAS value of the UTXO consumed by the active input.
            // Type: int.
self.state  // Complete typed source-level state owned by the actor.
            // Type: the state named in the actor's owns clause.
self.cov_id // Covenant ID carried by the active input. Type: cov_id.
            // Lowers to OpInputCovenantId(this.activeInputIndex).
self.type   // Reserved.
self.ref    // Reserved.
```

`self.cov_id` identifies the runtime covenant instance to which the active actor
input belongs.

An actor's effective top-level state cannot declare `state`, `value`, `cov_id`,
`type`, or `ref` as a field. This rule also applies to base fields and expansion
slots that are exposed by an expanded owned state.

The rule does not apply to fields of a nested state value. For example, the
`value` field below is valid because it is accessed through `payload`:

```rust
state Payload {
    int value;
}

state WalletState {
    Payload payload;
}
```

## Template hash rule

Argent uses Silverscript's template hash, which excludes all instance state,
including compiler-owned state:

```text
template_hash = blake2b(i64le(template_prefix.length) || template_prefix || i64le(template_suffix.length) || template_suffix)
```

The state bytes live between prefix and suffix, so template references stored in
state do not participate in their own template hash.

## Hidden ABI state

Template and route fields are compiler-owned ABI state, not source-level user
fields.

The compiler adds fields such as these when an actor needs route context:

```text
gen__player_template
gen__game_template
gen__mux_routes_digest
```

The `gen__` namespace is reserved. The artifact records each generated field's
role, and `argent-runtime` constructs its value from the template plan.
Same-context transitions preserve these fields; routes to another actor derive
the target actor's planned context.

## Transaction builder context

Argent source should not expose prefix/suffix witnesses, route proofs, template
preimages, or other Silverscript machinery. The portable artifact records
recipes for this hidden material. `argent-runtime`'s artifact-level `TxBuilder`
combines those recipes with a `TxContext` to construct actor inputs, outputs,
and entry arguments.

## Route commitments

The route planner builds a deterministic commitment forest. Each actor gets a
cut that contains concrete actor templates and packed family digests. A route
transition keeps common nodes, opens packed families, and packs families that
the target does not need open.

The implementation separates three parts:

```text
graph classification -> commitment forest and cuts -> SIL lowering
```

Graph classification defines the current family and cohort policy. Commitment
planning has no Argent AST or SIL data. SIL lowering uses each cut transition to
generate fields, table witnesses, hashes, and output checks.

The planner data model supports nested commitment branches. Compiler lowering
currently supports one-level family tables. The artifact records route tables,
family metadata, cut-based receipts, and witness recipes. `argent-runtime` uses
these receipts to fill hidden witnesses.

[Route Planning](route-planner.md) defines the terms, policy, algorithms, and
compiler boundary. [Routing Optimization Opportunities](../src/routing/optimization.md)
records known cases where the current policy produces correct but inefficient
cuts.

## Same-template shortcuts

Route lowering uses the strongest template identity already proved by the entry
model:

- An exact self-continuation with `self.state` compares the successor output's
  script public key with the active input's script public key.
- A same-actor continuation with new state uses `validateOutputState`.
- A foreign continuation can use `validateOutputStateWithInputTemplate` when a
  bound input already proves the target template.
- Other foreign continuations use `validateOutputStateWithTemplate` and a
  compiler-planned template witness.

Consumed and observed inputs follow the same rule. Argent uses
`readInputState` only when the entry model proves the current template is the
expected template; otherwise it uses `readInputStateWithTemplate`.

## Input and output shape

`consumes`, `emits`, `observes`, and `spawns` declare transaction shape. The
compiler lowers ordinary actor coordination as follows:

```text
leader input:    reads peer inputs through OpCovInput*
leader outputs:  validates successors through OpAuthOutput*
delegate inputs: verify they are not leader and require OpAuthOutputCount(active) == 0
```

Coordinated leader entries enforce the declared covenant input count and order.
Standalone entries that neither consume peers nor lead delegates allow
covenant batching. Delegates enforce a conservative minimum input count because
the leader may coordinate additional actors. Every entry enforces its exact
authorized output count and handle order.

An `observes` clause currently describes the complete observed covenant input
and output groups. A `spawns` clause describes an ordered genesis output group
and verifies its derived covenant ID. Flexible clause cardinality is specified
separately in [Entry clause ranges](entry-clause-ranges.md).

### Output value policy

Every output created by the current entry must reference its native value
explicitly. This includes ordinary `emits` handles and group-qualified spawn
handles:

```ag
require(next.value == self.value);
require(children.outputs.first.value > 0);
```

An intentionally free value uses `unrestricted(handle.value)`. The declaration
is compile-time-only and emits no Silverscript.

Observed outputs are excluded: their emitting contracts own their value policy.
The initial check is syntactic and entry-scoped. It requires each output value
reference to appear somewhere in the entry body; it does not prove that a
restriction is meaningful or present on every control-flow path.

## Body lowering

The compiler lowers entry bodies into plain Silverscript. Targeted lowering
handles terminal `become`, typed locals, `if/else`, state constructors,
transaction values, consumed and observed state, actor selectors, and generated
route validation.

Argent does not yet own a full source typechecker. The compiler performs the
analysis needed for lowering and leaves final helper/body validity to
Silverscript where possible.

## Application continuation invariants

Argent proves the declared actor, template, route, and successor-state
relationships. Application identity rules—such as preserving an owner, matching
a game identifier, or closing the intended live actors—remain explicit
`require(...)` conditions in source.

## Compiler obligations

Argent-generated code enforces the declared mechanical invariants:

- covenant input shape according to the leader, delegate, and batching rules
- exact authorized output shape where declared
- planned hidden ABI state preservation and transition
- typed foreign input template checks
- same-template output validation through `validateOutputState` where applicable
- route commitment membership checks
- successor state validation with the chosen template
- no `become` route outside the entry's declared `emits` set

Anything not generated must be obvious in source and reviewable as application
logic.
