# Argent

Argent is an actor-based language and compiler for building stateful,
multi-contract and multi-app applications on covenant-native UTXO rails.

Applications are expressed as transaction-wide state transitions over covenant
UTXOs. Actors own typed state, entries consume and emit actors, and `become`
defines the successor actors created by one atomic transaction. Inter-Covenant
Communication (ICC) extends the same model across independently compiled apps.

## Compiler foundation

Argent is built on [Silverscript](https://github.com/kaspanet/silverscript),
the core foundation of this compiler. Silverscript provides the complete
single-contract language and compiler stack, from typed contract source to
Kaspa Script, together with the low-level builtins that make Argent's complex
multi-contract and multi-app work possible.

Argent adds the application layer above that foundation. It turns `.ag` source
into plain, auditable Silverscript contracts and portable artifacts consumed by
`argent-runtime`. Together the layers handle state layouts, template
commitments, routing, output validation, cross-covenant observation, virtual
state expansion, and hidden witness material.

The design targets programmable UTXO systems with script-composition primitives
such as `OP_CAT` and `OP_SUBSTR`, transaction introspection, and
consensus-supported covenant identities (see
[Kaspa’s KIP-20](https://github.com/kaspanet/kips/blob/master/kip-0020.md) for a
reference model).

Kaspa is the native target: `.ag` programs compile to Silverscript and
ultimately execute on the native Kaspa Script engine. The language, artifact,
and runtime layers keep Kaspa-specific assumptions explicit and separated where
practical.

## Project status

> The project is still under active development and is not yet release-ready.
[Silverscript v1.0.0](https://github.com/kaspanet/silverscript/releases/tag/v1.0.0)
is released for production use, and Argent is pinned to that release. Advanced
users who can review the generated `.sil` contracts thus have a viable path to
careful early production use. This requires understanding Argent's route
semantics and compiler model well enough to verify that the generated contracts
match the intended application. Argent itself will still need further audit and
hardening before general production use.

The main pieces are present: compiler, generated Silverscript, portable
artifacts, runtime transaction building, multi-actor routing, cross-app linking,
constrained covenant spawning, actor enums, closed and open ICC, and
virtual-slot state expansion.

```text
.ag source
    |
    v
Argent compiler
    |
    +-- plain .sil contracts
    |
    +-- portable artifact
              |
              v
       argent-runtime
              |
              v
   atomic multi-actor Kaspa tx
```

## Quick start

Run the standard local check loop:

```sh
./check.sh
```

Regenerate tracked example outputs and run the full check loop:

```sh
./check.sh --full
```

Build the tracked examples manually:

```sh
cargo run -- build examples/tickets.ag --out examples/build/tickets
cargo run -- build examples/stones/app.ag --out examples/build/stones
cargo run -- build examples/icc/kcc20_asset.ag --out examples/build/icc_kcc20_asset
cargo run -- build examples/icc/minter.ag --out examples/build/icc_minter
cargo run -- build examples/open_icc/agent.ag --out examples/build/open_icc_agent
cargo run -- build examples/open_icc/core.ag --out examples/build/open_icc_core
```

When one source file declares multiple apps, select the app to build by name:

```sh
cargo run -- build contracts.ag --app DexCore --out build/dex-core
```

Generated outputs include:

- `artifact.json`: the portable Argent artifact
- `manifest.json`: build metadata
- `sil/*.sil`: generated Silverscript contracts

Inspect the compiled artifact without rebuilding it:

```sh
cargo run --bin argentc -- inspect examples/build/tickets
```

The report summarizes actor script, state, and template sizes, static opcode
counts, entry arguments and generated witnesses, route metadata, and
signature-script size estimates.

Generated `.sil` files compile as ordinary Silverscript. Within each contract,
Silverscript provides the types, expressions, functions, arrays, loops, and
control flow, then performs the final type checking and Kaspa Script
compilation. Argent does not use Silverscript covenant macros.

## Language at a glance

```rust
state EventState {
    int remaining_tickets;
    int price;
}

state TicketState {
    byte[32] owner;
}

actor Event owns EventState {
    entry buy(byte[32] buyer)
    // The `emits` clause declares the complete actor-output shape for this entry.
    emits {
        event: Event,
        ticket: Ticket,
    } {
        require(remaining_tickets > 0);

        // Every emitted output value must be constrained or explicitly unrestricted.
        require(event.value == self.value + price);
        unrestricted(ticket.value);

        EventState next_event = {
            remaining_tickets: remaining_tickets - 1,
            price: price,
        };
        TicketState new_ticket = { owner: buyer };

        // `become` binds each emitted handle to its successor actor and state.
        become {
            event <- Event(next_event),
            ticket <- Ticket(new_ticket),
        };
    }
}

actor Ticket owns TicketState {
    entry transfer(byte[32] next_owner, sig owner_sig, pubkey owner_pk) emits next: Ticket {
        // Prove ownership with a P2PKH-style public-key hash and signature.
        require(blake2b(byte[](owner_pk)) == owner);
        require(checkSig(owner_sig, owner_pk));
        unrestricted(next.value);
        TicketState new_state = { owner: next_owner };
        become next <- Ticket(new_state);
    }
}

// An app defines a covenant boundary, keeping the covenant state machine closed to these actors.
app Tickets {
    actor Event;
    actor Ticket;
}
```

One `Event::buy` transaction advances the Event and creates a separately owned
Ticket. Later transactions can transfer that Ticket independently.

Argent uses type-first syntax for declarations and callable parameters.
Bindings put the local name on the left. See
[Surface syntax conventions](docs/argent-design.md#surface-syntax-conventions)
for the rules and examples.

Argent actors are not async actors with mailboxes or message queues. They are
covenant objects that get consumed and recreated by transactions. The shared
idea with actor models is state ownership: an actor's code is the only
authority that can consume and mutate that actor's state.

Core terms:

- `state` defines a persistent covenant state layout.
- `actor` defines one contract template that owns a state layout.
- `entry` defines a callable transition path.
- `delegate` defines a non-leading check in a coordinated transition.
- `consumes` names peer covenant inputs in the same transaction.
- `emits` declares the authorized output handles for an entrypoint.
- `become` is the terminal transition into successor actor state.
- `observes` declares a foreign covenant view for ICC.
- `spawns` declares a genesis covenant output group and binds its generated
  covenant ID. A spawn target can be an actor in the selected app or an
  `actor_type<State>` value.
- `actor_type<State>` identifies a runtime-selected actor implementation
  compatible with `State`.
- `actor enum` defines a closed set of runtime-selected actor targets.
- `virtual` slots and `state X expands Base` let concrete actors bind private
  digest-backed memory while preserving a shared base state layout.
- `app` declares the closed actor set shared by its covenant instances.

## Examples

- [examples/tickets.ag](examples/tickets.ag): tiny single-file issuer/ticket app
- [examples/spawns.ag](examples/spawns.ag): constrained genesis covenant launch
  with a complete two-output group
- [examples/stones](examples/stones): small coordinated game with league,
  player, game, and settle actors
- [examples/toy_chess](examples/toy_chess/app.ag): actor enums and
  route-family selector lowering
- [examples/icc](examples/icc): closed ICC between a minter and asset app
- [examples/open_icc](examples/open_icc): open observed actors and virtual-slot
  agent state

For client-side examples, see
[argent-playground](https://github.com/argent-lang/argent-playground). It is a
separate Rust project that depends on a neighboring Argent checkout and shows
complete app compilation and transaction-building flows through
`argent-runtime`.

## Runtime

`argent-runtime` is the artifact-only consumer surface. It has no compiler
dependency. It loads compiled artifacts, fills hidden witness material, builds
covenant UTXOs, composes artifact bundles, and builds complete transactions
from concrete actor inputs and outputs.

Classic single-app flow:

```rust
let builder = TxBuilder::new(&artifact)?;

let before = state! { remaining_tickets: 10, price: price };
let after = state! { remaining_tickets: 9, price: price };
let ticket = state! { owner: buyer.clone() };

// Normally loaded from a Kaspa node or application storage.
let event_utxo = builder.covenant_utxo(
    "Event", // Actor type, specified by name.
    before.clone(),
    event_value,
    0,
    false,
    Some(covenant_id),
)?;

let context = TxContext::new()
    .actor_input(
        "Event",
        before,
        EntryCall::new("buy").args(args![buyer]),
        event_outpoint,
        event_utxo,
        0, // sequence
    )
    // The ordinary buyer input funds the price, Ticket value, and fees.
    .input(buyer_outpoint, buyer_utxo, buyer_sig_script, 0)
    // Both actor outputs continue under the Event input's covenant.
    .actor_output(
        "Event",
        after,
        CovenantBinding::new(0 /* Event input index */, covenant_id),
        event_value + price,
    )
    .actor_output(
        "Ticket",
        ticket,
        CovenantBinding::new(0, covenant_id),
        ticket_value,
    );

let tx = builder.build(&context)?;
```

Each input declares its sequence. Lock time, lane and gas, and payload can be
set fluently on `TxContext`; their defaults produce a native transaction.

The runtime API is Argent-specific while the language settles. The lower-level
Silverscript ABI and artifact boundaries are split into small crates so they can
be kept portable. Multi-app ICC uses `ArtifactBundle`; the transaction context
is otherwise the same for single- and multi-app transactions.

## Genesis proofs

A covenant ID commits to the authorizing outpoint and initial output group.
Genesis proofs let a verifier check which app and actor states started a
covenant, using its ID from a node-provided UTXO.

Verify an app and its bootstrap data against the on-chain ID:

```text
cargo run -- genesis verify --source app.ag \
  --bootstrap genesis.json --covenant-id <node-provided-id>
```

Portable proof packages can also be composed from source, compiled Argent
artifacts, or independent Silverscript ABI files. The `argent-genesis` crate
provides the Rust APIs, including bootstrap export from a launch `TxContext`.
See [Genesis proof tooling](docs/genesis-proof-tooling.md)
for bootstrap formats, examples, and the proof layers.

## Why Argent

Kaspa covenants make it possible to build applications from several stateful
UTXOs whose transitions compose atomically in one transaction. But hand-written
multi-contract systems quickly accumulate mechanical obligations: state
serialization, template hashes, route commitments, prefix/suffix witnesses,
output ordering, observed covenant IDs, and cross-contract state reads.

Argent makes the application graph source-level. Actors own state. Entries
declare the peer actors they consume, the outputs they emit, the foreign
covenants they observe, and the successor actors those outputs become. The
compiler checks the declared state-machine edges and emits the Silverscript that
performs the low-level validation.

Generated contracts stay as plain `.sil` files, and the artifact records the
runtime recipe needed to build transactions against them.

## How it works

The compiler parses `.ag` source into an actor/state model and lowers each actor
to one Silverscript contract. Source state fields become the contract state
layout.

A central compiler task is making each actor commit in advance to every actor
template it may create next. Argent derives these commitments from the app's
[route graph](docs/route-planner.md) and places the required context in generated
contracts, so entries can validate successor templates without trusting values
provided by callers.

Compiler-generated fields and hidden entry arguments carry template receipts,
route-family tables, observed-covenant witnesses, and expanded-state preimages.

`become` routes lower to output validation. Exact continuations can use cheaper
script-public-key checks. Foreign or runtime-selected actors use template
prefix/suffix witnesses or route-family tables. `observes` lowers to covenant
input/output checks against another app. `virtual` slots lower to fixed digest
fields, with concrete actors providing hidden preimages when they expand those
slots into structured memory.

The portable artifact records the runtime recipe for all of this: script bytes,
state layouts, type descriptors, route receipts, observed covenant metadata,
hidden witness recipes, artifact IDs, and interface fingerprints.
`argent-runtime` consumes that artifact directly; it does not depend on compiler
AST types.

Each app artifact also records the exact artifact ID of each direct app
dependency. Runtime bundles reject missing or different dependency artifacts
before they build a transaction.

## Current status

The core language, compiler, artifact, and runtime flows are functional and
covered by tracked end-to-end examples. Work before the first release is
focused on:

- completing ranged observe and spawn clauses
- refactoring code generation into a pure Argent-to-Silverscript AST
  transformation
- auditing and hardening the compiler, artifacts, and runtime

Design notes can be found in [docs/argent-design.md](docs/argent-design.md).
ICC semantics can be found in [docs/icc-semantics.md](docs/icc-semantics.md).
Subtle generated-code security arguments are documented in
[Security invariants](docs/security-invariants/README.md).

## Contributing

Run `./check.sh --full` before submitting changes. Open design questions and
implementation sketches are collected in [docs/followups.md](docs/followups.md);
they are useful starting points for discussion and contributions.
