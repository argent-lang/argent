# Entry ranges review

This report reviews `entry-ranges-part1` at commit `900b481d12ca` against
`master`. It focuses on the remaining issues in the first leader-consume and
current-emit range implementation.

The range model, count arithmetic, route-field provenance, generated output
loops, artifacts, and current runtime validation are otherwise coherent. The
main remaining problem is that a ranged consumed item does not follow Argent's
input-reference rules.

The distinction between an actor input reference and an authored state value
comes from the recent state-layout work on `master`.

The source-language rule is described in
[Input references and authored state](argent-design.md#input-references-and-authored-state).
The compiler implementation is described in
[Authenticated input boundary](compiler-design.md#authenticated-input-boundary).

## 1. [P1] Apply scalar input-reference semantics to ranges

### The new scalar distinction

A consumed handle identifies an actor input. It is not a state value:

```rust
consumes {
    peer: Peer,
}
```

The programmer can inspect the actor input directly:

```rust
int x = peer.x;
int value = peer.value;
cov_id id = peer.cov_id;
```

Complete authored state requires an explicit operation:

```rust
PeerState complete = state(peer);
```

The scalar codegen keeps the same distinction. It authenticates and reads the
physical state once:

```sil
Gen__PeerState gen__peer_state = readInputStateWithTemplate(
    gen__peer_input_idx,
    gen__peer_prefix_len,
    gen__peer_suffix_len,
    gen__peer_template
);
```

The physical value may contain compiler-owned route fields:

```sil
struct Gen__PeerState {
    byte[32] gen__archive_template;
    byte[32] gen__peer_template;
    int x;
}
```

A direct field read projects from that authenticated value:

```rust
int x = peer.x;
```

```sil
int x = gen__peer_state.x;
```

`state(peer)` constructs the authored value and strips the generated fields:

```rust
PeerState complete = state(peer);
```

```sil
PeerState complete = PeerState {
    x: gen__peer_state.x,
};
```

This reconstruction does not authenticate or read the input again. It uses the
already authenticated physical value. When physical and authored layouts are
identical, the compiler can use the cached value directly.

The result is one clear pipeline:

```text
peer                         actor input reference
  │
  │ authenticate and read once
  ▼
Gen__PeerState               authenticated physical state
  ├── peer.x                 direct authored-field projection
  └── state(peer)            explicit complete reconstruction
          │
          ▼
      PeerState              ordinary authored value
```

### Current range behavior and failures

For a ranged consume:

```rust
consumes {
    accounts: Account[1..=2],
}
```

the current implementation models `accounts` as an authored `AccountState[]`
body value rather than a range of actor input references. Ordinary array
indexing can therefore produce an authored value without calling `state(...)`.

Because the implementation maps each item to `AccountState`, an indexed item
can be passed directly where an authored value is expected:

```rust
require(balance_of(accounts[0]) >= 0);
```

The indexed expression behaves as an ordinary `AccountState` value. The
input-reference operations introduced on `master` do not work:

```rust
AccountState explicit = state(accounts[0]);
require(accounts[0].cov_id == accounts[0].cov_id);
```

Current diagnostics are:

```text
`state(...)` requires one visible entry input reference, but `accounts[0]` is not one
```

and:

```text
struct 'AccountState' has no field 'cov_id'
```

Code generation and body metadata also disagree about the range type. The
generated prelude uses `AccountState[]`, while body lowering records
`Gen__AccountState[]`.

In the [complete reproducer](#route-bearing-indexed-input), this line:

```rust
AccountState first = accounts[0];
```

produces:

```text
variable 'first' expects Gen__AccountState
```

Neither array type describes the source semantics:

```text
generated prelude   accounts is AccountState[]
binding metadata    accounts is Gen__AccountState[]
language model      accounts[i] must be an input reference
```

### Required ranged semantics

Apply the scalar rule without adding a new exception. The programmer sees
`accounts` as a range of consumed `Account` actor inputs:

```text
accounts                     actor input reference range
accounts[i]                  one Account input reference
state(accounts[i])           one AccountState authored value
```

The supported operations are:

```rust
accounts.length
accounts[i].balance
accounts[i].value
accounts[i].cov_id
state(accounts[i])
```

The body model should record a homogeneous range of indexed input references
with one known actor and source-state type. It should not expose `accounts` as
a normal Sil state array.

Reject bare indexed references where an authored value is required:

```rust
AccountState value = accounts[i];
require(balance_of(accounts[i]) >= 0);
```

The diagnostic should identify `accounts[i]` as an input reference and suggest
`state(accounts[i])`.

Accept the explicit forms:

```rust
AccountState value = state(accounts[i]);
require(balance_of(state(accounts[i])) >= 0);
```

All operations must share the same index lowering and bounds rules.

### Recommended codegen

The current bounded loop already does the important physical-to-authored work:
it authenticates each physical input, projects its authored fields, and strips
its compiler-owned route fields. The consuming contract does not use those
fields, so they indeed do not belong in the authored cache. Keep the existing
work, but append the result to a hidden compiler cache rather than exposing the
cache as the source value of `accounts`:

```sil
AccountState[] gen__accounts_authored;

for (gen__accounts_position, 0, gen__accounts_count, 2) {
    int gen__accounts_input_idx = OpCovInputIdx(
        gen__cov_id,
        1 + gen__accounts_position
    );

    Gen__AccountState gen__account_physical = readInputStateWithTemplate(
        gen__accounts_input_idx,
        gen__account_prefix_len,
        gen__account_suffix_len,
        gen__account_template
    );

    AccountState gen__account_authored = AccountState {
        balance: gen__account_physical.balance,
    };

    gen__accounts_authored =
        gen__accounts_authored.append(gen__account_authored);
}
```

Body lowering then uses the hidden cache:

```text
accounts.length
    → gen__accounts_count

accounts[i].balance
    → gen__accounts_authored[checked(i)].balance

state(accounts[i])
    → gen__accounts_authored[checked(i)]
```

Native input operations still use the transaction input index:

```text
accounts[i].value
    → tx.inputs[input_index(checked(i))].value

accounts[i].cov_id
    → OpInputCovenantId(input_index(checked(i)))
```

This design makes sense for four reasons:

1. Each physical input is authenticated and read only once.
2. Generated route fields are not needed by the consuming contract and never
   enter an authored value.
3. `state(accounts[i])` remains explicit in Argent even when codegen can use a
   cached authored value directly.
4. The implementation remains close to the existing bounded loop.

### Architecture recommendation

Extend the existing entry input-reference plan. Do not add another state-array
type path to `BodyLowerer`.

One possible shape is:

```text
EntryInputReferencePlan
  └── consumed "accounts"
        └── PlannedInputReferenceRange
              ├── actor and source-state identity
              ├── count, bounds, and covenant position
              ├── hidden authored-cache expression
              └── item(checked_index)
                    └── IndexedInputReference
```

`IndexedInputReference` should expose the same operations as the existing
scalar reference:

```text
project_field(name)
complete_authored_state()
native_value()
covenant_id()
```

The implementation responsibilities then remain clear:

- The emitter authenticates each physical input and builds the hidden authored
  cache.
- `BodyLowerer` recognizes `accounts[i]` syntax and lexical visibility.
- The range plan validates the index and derives the transaction input index.
- The indexed reference delegates field projection and complete authored-state
  access to the existing state-boundary machinery.

The ordinary body binding table should record that `accounts` is an input
reference range. It should not assign `accounts` a source or lowered Sil array
type. The hidden `AccountState[]` cache is a separate compiler expression.

Merely changing `lowered_type` to `AccountState[]` would hide one diagnostic
without fixing the language model.

### Required coverage

Add focused tests for a route-bearing consumed actor:

```rust
require(accounts[i].balance >= 0);
require(accounts[i].value >= 0);
require(accounts[i].cov_id == expected_cov_id);

AccountState value = state(accounts[i]);
require(balance_of(state(accounts[i])) >= 0);
```

Also reject:

```rust
AccountState value = accounts[i];
require(balance_of(accounts[i]) >= 0);
```

Cover literal and variable indices, including the existing runtime bounds
check.

## 2. [P1] Expanded consumed ranges

An expanded input may contain a stored digest without the validated preimage
needed to build its complete authored state. Such a range cannot always use the
authored cache above.

This needs an explicit scope decision. The simplest deferral is to reject
expanded consumed ranges at declaration validation with a direct diagnostic.
The current implementation instead fails later while generating the prelude,
even when the body only uses `foragers.length`. Deferral would also block the
intended KCC20 prototype because the KCC20 state contains a virtual field.

If we choose to support expanded consumed ranges as part of this work, they
need a physical cache or another projection-oriented backing. Ordinary
available fields, `.value`, and `.cov_id` can then work, while
`state(foragers[i])` and unavailable expanded fields remain rejected.

Add a focused diagnostic test for the selected behavior.

## 3. [P2] `unrestricted(...)` accepts an impossible ranged-output item

For this output:

```rust
emits {
    next: Account[1..=3],
}
```

the following declaration currently compiles:

```rust
unrestricted(next[3].value);
```

Valid indices are `0`, `1`, and `2`. Ordinary `next[3].value` expressions are
rejected by the range-index lowering path, but
`validate_unrestricted_output_value` only checks whether the expression
syntactically names the output handle. It does not validate the index.

This matters because one indexed `unrestricted(next[i].value)` currently
satisfies the handle-level value policy for the complete range. An impossible
item must not satisfy that policy.

Expected behavior:

- Reject a literal index below zero or greater than or equal to the declared
  maximum.
- Accept an in-range literal index.
- Accept a variable index as a declaration for the handle under the current
  handle-level policy. `unrestricted(...)` emits no runtime code.
- Keep the current handle-level value-disposition policy. Per-item coverage is
  a separate follow-up.

The declaration validator and ordinary lowering must agree about which literal
indices can exist. Actual emitted uses of variable indices continue through the
existing runtime bounds check.

## 4. [P2] Add a runtime range transition test

The compiler fixtures pin generated Sil, and runtime unit tests validate
artifact counts. No end-to-end transaction test currently executes the new
bounded input and output loops.

Add one route-bearing flow that consumes and emits a range. It should prove
that authored fields move through the transition while compiler-owned route
fields come from the authenticated route plan.

Cover these lengths:

```text
minimum
one middle value
maximum
minimum - 1
maximum + 1
```

The first three must execute successfully. The last two must fail covenant
execution or transaction construction at the intended stage. The test
should also modify an authored field so it proves real state materialization,
not only unchanged state forwarding.

---

## Documentation corrections

After the implementation is fixed, update `entry-clause-ranges.md`:

- Replace the statement that a range array contains authored state values with
  the indexed input-reference rule.
- Document the selected expanded-range behavior. Rejecting expanded consumed
  ranges is an explicit deferral and blocks the KCC20 prototype because its
  state contains a virtual field.
- Keep `state(remote.inputs.assets[i])` as the intended future observed-range
  syntax.
- Remove the planned artifact schema-version increment. Argent is pre-release
  and does not preserve the current schema.

The general input-reference rules in `argent-design.md` and
`compiler-design.md` remain authoritative.

## Complete reproducers

These programs are self-contained and can be compiled as separate Argent
sources.

### Uniform indexed input-reference behavior

The following program should compile after the fix. It currently rejects
`state(accounts[0])` because the indexed item is not recognized as an input
reference.

```rust
state BatchState {}

state AccountState {
    int balance;
}

fn balance_of(AccountState value) -> int {
    return value.balance;
}

actor Batch owns BatchState {
    entry inspect()
    consumes {
        accounts: Account[1..=2],
    }
    emits none {
        require(accounts.length >= 1);
        require(accounts[0].balance >= 0);
        require(accounts[0].value >= 0);
        require(accounts[0].cov_id == accounts[0].cov_id);

        AccountState current = state(accounts[0]);
        require(balance_of(state(accounts[0])) >= 0);
        require(current.balance >= 0);
    }
}

actor Account owns AccountState {
    entry hold() emits none {
        require(balance >= 0);
    }
}

app Test {
    actor Batch;
    actor Account;
}
```

The current implementation also accepts both invalid forms in this program:

```rust
AccountState current = accounts[0];
require(balance_of(accounts[0]) >= 0);
```

The diagnostic should say that `accounts[0]` is an input reference and suggest
`state(accounts[0])`.

### Route-bearing indexed input

This program exposes the conflicting physical binding metadata. The valid
explicit reconstruction should compile after the fix and must not include
`Account`'s generated route fields in `first`.

```rust
state BatchState {}

state AccountState {
    int balance;
}

actor Batch owns BatchState {
    entry inspect()
    consumes {
        accounts: Account[1..=2],
    }
    emits none {
        AccountState first = state(accounts[0]);
        require(first.balance >= 0);
    }
}

actor Account owns AccountState {
    entry archive() emits next: Archive {
        AccountState next_state = {
            balance: balance,
        };

        unrestricted(next.value);
        become next <- Archive(next_state);
    }
}

actor Archive owns AccountState {
    entry hold() emits none {
        require(balance >= 0);
    }
}

app Test {
    actor Batch;
    actor Account;
    actor Archive;
}
```

For comparison, this legacy form currently reports that `first` expects
`Gen__AccountState`:

```rust
AccountState first = accounts[0];
```

After the fix it should instead report that complete authored state requires
`state(accounts[0])`.

### Expanded range decision

This program pins the scope decision. If expanded ranges are deferred, it
should reject the range declaration with a direct unsupported-feature
diagnostic. If they are supported in this work, it should compile because the
body uses only available input-reference operations. It currently fails later
while the generated prelude attempts complete authored reconstruction.

```rust
state BatchState {}

state AgentCapsule {
    virtual strategy;
    int energy;
}

state ForagerStrategy {
    int hunger;
}

state ForagerState expands AgentCapsule {
    strategy: ForagerStrategy;
}

actor Batch owns BatchState {
    entry inspect()
    consumes {
        foragers: Forager[1..=2],
    }
    emits none {
        require(foragers.length >= 1);
        require(foragers[0].energy >= 0);
        require(foragers[0].value >= 0);
        require(foragers[0].cov_id == foragers[0].cov_id);
    }
}

actor Forager owns ForagerState {
    entry hold() emits none {
        require(energy >= 0);
    }
}

app Test {
    actor Batch;
    actor Forager;
}
```

Even when expanded ranges are supported, complete reconstruction remains
unavailable without a validated opening. This must reject with a direct
diagnostic:

```rust
ForagerState complete = state(foragers[0]);
```

### Impossible `unrestricted(...)` item

This program currently compiles. It must reject `next[3]` because a range with
maximum cardinality `3` has only positions `0..2`.

```rust
state BatchState {}

state AccountState {
    int balance;
}

actor Batch owns BatchState {
    entry produce(AccountState[] states)
    emits {
        next: Account[1..=3],
    } {
        unrestricted(next[3].value);
        become next <- Account[](states);
    }
}

actor Account owns AccountState {
    entry hold() emits none {
        require(balance >= 0);
    }
}

app Test {
    actor Batch;
    actor Account;
}
```
