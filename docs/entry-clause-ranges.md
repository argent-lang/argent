# Ranges in entry clauses

> This document defines the shared range model and its staged implementation.
> Leader consumes and current-entry emits are implemented first; observed and
> spawned ranges remain explicit follow-up stages below.

## Purpose

Argent entry clauses describe an ordered transaction shape. Each declared
handle currently describes one input or one output. Some protocols need a
bounded number of items of the same actor type. A range lets one handle
describe that ordered group.

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

Observed and spawned ranges reserve the same source form, but the current
compiler rejects them at code generation until their transaction rules are
implemented:

```rust
observes assets by self.asset_id {
    inputs {
        previous: Asset[1..=MAX_ASSETS],
    }
    outputs {
        next: Asset[1..=MAX_ASSETS],
    }
}

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

A future observed-range stage will derive each actual length from
`OpCovInputCount` or `OpCovOutputCount`, materialize observed input states in a
bounded loop, and validate observed output state arrays in a bounded loop.

The runtime must change its observed context from one item per handle to one or
many items per handle. Fixed actor ranges can share one template witness.

Input-template reuse needs a clear rule. The first implementation should use a
shared output template witness for an observed output range. A later
optimization can reuse one matching observed input when the input range cannot
be empty.

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

The first compiler slice supports leader `consumes` ranges and current
`emits` ranges with one fixed actor target. It supports singleton items before
and after either range. Delegate consumes, observed ranges, spawned ranges,
and their runtime transaction construction remain follow-up work. The runtime
rejects artifacts that claim observed or spawned ranges until those paths are
implemented; it does not silently interpret them as singleton declarations.
Artifact attachment also rejects more than one range on either supported
current-covenant side and rejects ranged emits without exactly one fixed actor,
mirroring the compiler's first-version shape.

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
- an observe range adds no app route relation.

The commitment forest and cut transitions therefore stay unchanged.

The body lowerer needs explicit range bindings. Its current text replacement
is not sufficient for indexed expressions such as `accounts[i].value`,
`remote.inputs.assets[i].amount`, or `state(remote.inputs.assets[i])`. Add
token-aware indexed access lowering. A
full expression type checker is not required for the first version.

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

- Generate ranged observed input and output loops.
- Add vector entries to observed runtime contexts.
- Update hidden template resolution and observed output field witnesses.
- Add closed-ICC runtime tests first. Add shared open actor types later.

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
