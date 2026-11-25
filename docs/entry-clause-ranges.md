# Ranges in entry clauses

> This document defines the shared range model and its staged implementation.
> The compiler supports leader consumes, current-entry emits, and fixed-actor
> observed ranges. Spawned ranges are a follow-up stage below.

## Purpose

Argent entry clauses describe an ordered transaction shape. A singleton
handle describes one input or output. A range handle describes a bounded
number of items of the same actor type.

The compiler lowers a range to a bounded `for` loop and private authenticated
caches. The transaction sets the actual length. A compile-time upper bound
limits the generated script.

This feature is possible with the current Sil backend. It is not only a parser
change. It affects entry body types, transaction locations, artifact metadata,
hidden witnesses, and runtime resolution.

## Recommended source model

Use the current singleton form for one item:

```rust
consumes {
    owner: Account,
}
```

Use a cardinality suffix for a range:

```rust
const int MAX_ACCOUNTS = 8;

consumes {
    accounts: Account[1..=MAX_ACCOUNTS],
}
```

The lower and upper bounds are inclusive. Both bounds must be compile-time
integers. A consumed range handle is a collection of actor input references,
not an authored state array. `accounts[i]` is one input reference;
`state(accounts[i])` explicitly reconstructs its authored state.

The same cardinality syntax and artifact model cover all ordered entry
sections:

```rust
entry rebalance(AccountState[] next_states)
consumes {
    accounts: Account[1..=MAX_ACCOUNTS],
}
emits {
    next: Account[1..=MAX_ACCOUNTS],
} {
    require(accounts.length == next_states.length);
    become next <- Account[](next_states);
}
```

An emitted range uses one bulk `become` route. The `Actor[]` suffix marks the
route as bulk, and its state expression is an array. The compiler validates one
output for each array item.

Observed inputs and outputs use the same range syntax:

```rust
observes assets by self.asset_id {
    inputs {
        previous: Asset[1..=MAX_ASSETS],
    }
    outputs {
        next: Asset[0..=MAX_ASSETS],
    }
}
```

Pass a named authored state array to the bulk route:

```rust
require assets.outputs become {
    next <- Asset[](next_states),
};
```

Spawned outputs reserve the same source form, but the compiler does not yet
support them:

```rust
spawns batch by batch_id {
    outputs {
        items: Item[1..=MAX_ITEMS],
    }
}
```

## Terms

- A **singleton** has cardinality one.
- A **range** has a compile-time minimum and maximum.
- The **actual length** is the item count in one transaction.
- A **range handle** is the shared source name for the items.
- A **section** is one ordered list, such as `consumes`, `emits`, or one
  `observes.inputs` list.
- A **template constant** is a value that fixes generated code and the actor
  template hash. It is not actor state and it is not an entry argument.

Do not use *range* for an actor union. An actor union selects a type. A range
selects a number of items.

## Semantic rules

The following rules apply to each range:

1. The transaction supplies the actual length.
2. The generated script requires `minimum <= actual length <= maximum`.
3. The generated script covers every item exactly once and in transaction
   order.
4. Every item has the actor type that the declaration permits.
5. Every input state read uses the required template check.
6. Every output state and template is validated.
7. The upper bound is part of the actor template. State cannot change it.
8. A bulk route state array has the same length as its output range.

Input-reference ranges and authored output arrays stay on opposite sides of
the state boundary:

- `accounts[i]` is an authenticated actor input reference. Direct field
  projection, `.value`, and `.cov_id` are input-reference operations.
- `state(accounts[i])` is required to obtain one complete authored state.
  Passing a bare indexed reference where authored state is expected is an
  error.
- The compiler caches only authenticated authored projections. Compiler-owned
  route fields remain private; a same-actor ranged output does not inherit
  them from the input range.
- Input and output ranges are independent. A one-for-one application
  transition must explicitly require equal lengths; actor identity alone does
  not establish positional correspondence.
- Expanded consumed states allow projections whose values exist in the
  authenticated physical state. A virtual field or complete `state(...)`
  reconstruction is rejected when no validated preimage is available.
- Bulk output routes still consume ordinary authored state arrays. Each output
  element is materialized independently through the target state boundary.

The first version must not use a consume range in a delegate. A delegate's
covenant group can contain peer delegates that its clause does not name. The
complete group count therefore does not give the consume range length. A later
version needs an explicit partition length for this case. A delegate's first
`consumes` item names its leader; a leader entry's active input is not part of
its `consumes` section.

A spawn clause must have a minimum total cardinality of at least one. An empty
genesis group has no output from which the script can prove its covenant ID. A
range in that clause can have a minimum of zero when a singleton output keeps
the clause nonempty.

## First-version boundary

Allow at most one range in each section. Singleton items can occur before and
after the range.

This rule makes the range length derivable from the complete section item
count:

```text
actual range length = group count - singleton count
```

For a leader `consumes` section, the section item count is
`OpCovInputCount(cov_id) - 1`. It excludes the active leader input. Auth output
and observed section counts use their complete group counts.

The compiler can then calculate every item location without a length witness.
For a range in the middle, the fixed prefix and fixed suffix determine its
start and end.

Two ranges in one section need extra partition data. The total group count does
not show where the first range ends. A later version can add named partition
lengths to the transaction context and hidden ABI. Do not add that mechanism
before a use case needs it.

The first version must also require one fixed actor type for all items in a
range. Defer these forms:

- one actor-enum selector for each item;
- one open `actor_type` value for each item;
- different actor types inside one range.

A later version can support one shared selector or one shared open actor type
for the complete range. Per-item selectors are a separate feature.

## Compile-time bounds

The first version accepts integer literals and top-level `const int` values
initialized with integer literals. General constant expressions remain a
follow-up. Argent requires:

```text
0 <= minimum <= maximum <= 512
```

The compiler limit bounds Sil's compile-time loop expansion and protects
compiler and script-size budgets. Sil already treats a contract constant as a
compile-time loop bound. This is enough for the first version.

Sil also treats constructor arguments as compile-time constants. Argent can
later add configurable template constants and emit them as non-state Sil
constructor arguments. The artifact must record their resolved values. A
change to such a value changes the compiled template and its hash.

Do not use a source state field as a range bound. Argent currently compiles
state constructor arguments with placeholder values when it builds a template.
A state field is also mutable. It cannot safely control loop unrolling or
transaction shape.

## Sil lowering

### Consumes

For this declaration:

```rust
consumes {
    accounts: Account[1..=MAX_ACCOUNTS],
}
```

the leader prelude has this shape:

```sil
int account_count = OpCovInputCount(cov_id) - 1;
require(account_count >= 1);
require(account_count <= MAX_ACCOUNTS);

AccountState[] gen__accounts_authored;
for (i, 0, account_count, MAX_ACCOUNTS) {
    int input_idx = OpCovInputIdx(cov_id, 1 + i);
    Gen__AccountState physical = readInputStateWithTemplate(
        input_idx,
        account_prefix_len,
        account_suffix_len,
        account_template
    );
    AccountState authored = AccountState {
        balance: physical.balance,
    };
    gen__accounts_authored = gen__accounts_authored.append(authored);
}
```

The cache is compiler-private: there is no body-visible `AccountState[]`
binding named `accounts`. The physical read authenticates every input and the
cache strips compiler-owned route fields from authored access. Ranged outputs
materialize their compiler-owned fields independently from the compiler's
planned target context, following the same rule as scalar outputs.

Body operations lower as follows:

```text
accounts.length          -> gen__accounts_count
accounts[i].balance      -> gen__accounts_authored[checked(i)].balance
accounts[i].value        -> tx.inputs[OpCovInputIdx(... checked(i))].value
accounts[i].cov_id       -> OpInputCovenantId(OpCovInputIdx(... checked(i)))
state(accounts[i])       -> gen__accounts_authored[checked(i)]
```

For an expanded state, the compiler uses private per-field caches for fields
available from authenticated storage. It does not fabricate missing virtual
preimages.

The compiler rewrites `accounts[i].value` to the transaction input at
`OpCovInputIdx(cov_id, 1 + gen__checked_range_index(i, account_count))`. Literal
indices that are guaranteed by the minimum cardinality can omit the runtime
check. The existing single-actor direct-read optimization can apply to each
loop item when its current security rule applies.

### Emits

For a ranged output, the generated code checks the output count and the state
array length. It then validates each output:

```sil
int next_count = OpAuthOutputCount(this.activeInputIndex);
require(next_count >= 1);
require(next_count <= MAX_ACCOUNTS);
require(next_states.length == next_count);

for (i, 0, next_count, MAX_ACCOUNTS) {
    int output_idx = OpAuthOutputIdx(this.activeInputIndex, i);
    validateOutputStateWithTemplate(
        output_idx,
        next_states[i],
        account_prefix,
        account_suffix,
        account_template
    );
}
```

The exact validation builtin still depends on the current route rules. A same
template route can use `validateOutputState`. A route that can reuse an input
template can use `validateOutputStateWithInputTemplate`. This reuse is valid
only when the matching input range cannot be empty. Otherwise, use a shared
output template witness.

In the entry body, `next.length` is the actual output count. Indexed
`next[i].value` access uses the same generated bounds check as ranged input
values.

### Observes

An observed input range uses `OpCovInputCount(observed_id)` minus the number
of singletons on that side. Unlike `consumes`, there is no active-input offset.
The compiler authenticates each item and caches only the authored fields it
can read from the authenticated state. For example, use
`assets.inputs.previous.length`, `assets.inputs.previous[i].value`, or
`state(assets.inputs.previous[i])`. Whole-state reconstruction still requires
validated openings for all expanded fields.

An observed input range must name a fixed actor, in this app or a linked app,
and have a minimum of at least one. This guarantees a first input: a matching
output can reuse its authenticated template, just as with a singleton input.
If a range could be empty, an output could not rely on it for a template proof.
Open ranged targets are not yet supported. A shared `as` binding would also
need a first input to bind.

An observed output range uses `OpCovOutputCount(observed_id)` minus the number
of singleton outputs. The count includes continuations of the observed covenant
regardless of which input authorizes them. It excludes genesis outputs. The
bulk route checks that the state array has the same length, then materializes and
validates each element at its `OpCovOutputIdx` position. A matching,
guaranteed input can provide the template for all elements. Otherwise, the
outputs share one template witness when required.

The output minimum can be zero: the array must then be empty when there are
no output members. This does not change the requirement to name the range in
the `require ... outputs become` block. The runtime still requires at least
one observed member. The emitting contracts still own their output-value
checks.

To measure the cost of observed input ranges, run
`cargo run --locked --example observed_range_cost`. It varies the declared
maximum and actual count independently and reports the observer's compiled
bytecode size and charged script units. The fixture caches one `int` per input
and has no observed outputs, so the results do not include output validation.

### Spawns

A future ranged-spawn stage needs one dynamic array of global output indices.
The runtime already knows the complete named `spawn::<clause>` group, so it can
supply this array without a new user-facing group API.

The generated script must:

1. Check the index array length against the range bounds.
2. Check that the indices are in strictly increasing order.
3. Rebuild the canonical genesis covenant-ID preimage in a bounded loop.
4. Check the derived covenant ID against a selected group output.
5. Validate the actor state and template at every selected output.

This preserves the current complete-group proof. It also supports unrelated
transaction outputs between spawn group members.

Before enabling ranged spawns, require the resolved minimum cardinality of the
complete genesis group to be positive. A group with no output cannot carry the
covenant-ID proof.

## Current implementation boundary

The compiler and runtime support leader `consumes` ranges, current `emits`
ranges, and fixed-actor observed input and output ranges. Singleton items can
appear before and after a range. Delegate consume ranges, open observed
ranges, and spawned ranges are not yet supported. Artifact checks reject
unsupported shapes rather than interpreting them as singleton declarations.

Ranged output-value policy is currently handle-level. One indexed
`next[i].value` reference, including `unrestricted(next[i].value)`, satisfies
the policy for the complete range. Argent does not yet verify value disposition
for every emitted item.

## Compiler architecture

Add cardinality to the semantic model. Do not expand a range into its maximum
number of singleton declarations.

One common model can serve all four clauses:

```text
Cardinality
  One
  Range { minimum, maximum }

Effect item
  name
  actor expression
  cardinality
  declaration position
```

Each lowering stage must use an ordered location plan. A location plan gives a
singleton index or a range start and actual length. It must be separate from
actor route planning.

The route graph does not need multiplicity. Add one graph relation for a range:

- a consume range adds one consume relation;
- an emit range adds one emit relation;
- a fixed-actor spawn range adds one emit relation;
- an observe range uses the same route relations as a singleton of that actor.

The commitment forest and cut transitions therefore stay unchanged.

The body lowerer resolves range expressions to declared interaction handles.
It uses those bindings to lower indexed expressions such as
`accounts[i].value`, `remote.inputs.assets[i].amount`, and
`state(remote.inputs.assets[i])` with the planned index checks.

Add structured `for` statement support to the Argent body lowerer. Users need
loops to calculate totals and build successor state arrays. Keep bulk `become`
terminal and outside a user loop. This keeps terminal route analysis simple.

## Artifact and runtime changes

The Argent artifact must record cardinality for every consume, emit output,
observed item, and spawn output. It must record declaration position instead
of assuming that every item has one fixed transaction index.

The Argent artifact schema now records this cardinality directly. No
compatibility version bump is needed before the first release. The Sil ABI
schema only needs a change if a required array type is not represented by its
current `DynamicArray` and `Struct` forms.

Add range subjects and purposes for hidden parameters only where they are
needed. A ranged spawn needs an output-index array. A homogeneous range must
share template witnesses. It must not receive one template witness for each
possible item.

Across the completed and planned stages, the runtime must:

- partition an ordered group into singleton items and its one range;
- validate actor metadata for every ranged item;
- expose observed ranges as ordered vectors;
- resolve a ranged spawn to an ordered output-index array;
- encode hidden arrays through the existing Sil ABI array support;
- report minimum, maximum, and actual lengths in errors.

The transaction builder does not need a new range-group API while each section
has at most one range. Transaction order and the fixed items determine the
partition.

## Implementation plan

### 1. Prove the backend shapes

Add hand-written Sil tests for:

- a foreign input state array read in a bounded loop;
- an auth output state array validated in a bounded loop;
- an observed covenant input and output range;
- a genesis covenant-ID preimage built from an index array.

Use constructor constants in one backend test. This confirms that a bound is
part of the compiled template and not its state span.

### 2. Add cardinality and bound evaluation

- Parse `Actor[min..=max]` in entry clauses.
- Add one shared cardinality type to the AST and semantic model.
- Evaluate integer literals and `const int` references.
- Reject invalid bounds and more than one range in a section.
- Reject consume ranges in delegate entries.
- Keep singleton syntax and behavior unchanged.

### 3. Add ordered location plans

- Plan singleton and range locations for covenant inputs, auth outputs,
  observed groups, and spawn groups.
- Use the plan for count checks and generated index expressions.
- Add unit tests for a range before, between, and after singleton items.

### 4. Add body range support

- Register consumed ranges as homogeneous input-reference collections.
- Lower `.length`, indexed field, `.value`, `.cov_id`, and `state(...)`
  operations through one checked-index path.
- Lower indexed observed field access and authored reconstruction such as
  `remote.inputs.assets[i].amount` and `state(remote.inputs.assets[i])`.
- Lower state arrays for route targets that contain hidden route fields.
- Add structured `for` statements with compile-time maxima.
- Add bulk range routes and terminal coverage checks.

### 5. Implement consumes and emits

- Generate bounded input reads and output validations.
- Preserve delegate leader rules.
- Preserve route transition and template-witness selection.
- Add one pinned generated Sil fixture and runtime tests with several valid
  lengths and both invalid bounds.

This is the best first usable slice. It proves the common model before ICC and
genesis rules add more cases.

### 6. Implement observes

- Authenticate fixed-actor input ranges and reuse their first input as a
  template proof where applicable.
- Validate fixed-actor output ranges in bounded loops.
- Partition observed sides in transaction order in the runtime.
- Leave shared open actor types and their witnesses for a later stage.

### 7. Implement spawns

- Add the ranged spawn index-array witness.
- Generate the dynamic canonical preimage.
- Update spawn artifact verification and runtime group resolution.
- Add direct generated-Sil security tests for missing, duplicate, reordered,
  and substituted indices.

### 8. Stabilize the interfaces

- Keep the pre-release Argent artifact schema version unchanged while the
  cardinality shape is still unreleased.
- Document the transaction order rules.
- Pin representative artifacts and generated Sil.
- Run `./check.sh --full` and the Argent Playground checks.
- Measure script size and charged operations at each supported maximum.

## Test matrix

Each clause needs tests for these lengths:

```text
minimum - 1
minimum
one middle value
maximum
maximum + 1
```

Use `minimum - 1` only when the minimum is greater than zero. Use a middle
value only when one exists.

Also test:

- zero length when the minimum is zero;
- a range between two singleton items;
- a wrong actor at the first, middle, and last range position;
- a wrong state at the first, middle, and last range position;
- a multi-actor app with foreign templates;
- a singleton app to prevent leakage of its direct-read optimization;
- route-family opening and packing inside a ranged emit;
- interleaved global output indices for a ranged spawn;
- artifact rejection for malformed cardinality or range witness metadata;
- unchanged generated Sil for entries that use only singletons.

## Effort and risk

The parser and route-graph work is small. The full feature is large because it
crosses the compiler and runtime boundary.

| Part | Size | Main reason |
| --- | --- | --- |
| Syntax, bounds, and semantic model | Small to medium | A shared model is direct, but bounds need compile-time evaluation. |
| Ordered location planning | Medium | Every section must use the same partition rules. |
| Body arrays and loops | Large | Current lowering is mostly scalar and uses text replacement. |
| Consumes and emits | Medium to large | These establish the common input and output model. |
| Observes | Large | Observed contexts and witnesses are scalar today. |
| Spawns | Large and security-sensitive | The canonical genesis proof must use a runtime index array. |
| Route planner | Small | Multiplicity does not change graph topology. |
| Artifact and runtime | Medium to large | Cardinality and vector contexts are new public data. |

A safe consumes-and-emits slice is approximately 8 to 12 focused commits. All
four clauses, runtime support, pinned Sil, and adversarial tests are
approximately 18 to 25 focused commits.

The main technical risk is script growth. Sil unrolls the loop maximum. A large
maximum repeats state reads, template checks, and output validation code. The
implementation must measure script size and operation cost before it selects
default or documented maximum values.

## Recommendation

Implement consumes and emits first. Use one homogeneous range per section and
top-level `const int` bounds. Keep the route planner unchanged apart from its
input adapter. Add observes after the body and location models are stable. Add
spawns last because their proof is the most security-sensitive.

Do not add configurable constructor constants in the first range commit.
Existing source constants already give Sil a compile-time loop bound. Add
template configuration only when an app must compile the same source with
different maxima.
