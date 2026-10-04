# Genesis proof tooling

Genesis proof tools explain how an existing covenant ID was created. A
covenant ID commits to the authorizing input's previous outpoint (ensuring
global uniqueness) and the complete genesis output group.
Each output commitment includes its transaction index, value, and script
public key. For a contract output, the script fixes the initial contract and
its state.

Checking this preimage establishes which contracts and initial states started
the covenant. The continuation contracts then govern what can follow. A
verifier starts with a covenant ID obtained independently, normally from a
node-provided UTXO, and checks the supplied bootstrap against it.

The proof follows the same lowering path as contract construction:

```text
Argent source + dependency closure
                    |
                    v
          compiled Argent artifacts
                    |
         authored state -> physical state
                    v
             Sil genesis proof
                    |
       runtime state -> redeem script -> P2SH
                    v
        consensus genesis preimage
                    |
                    v
              covenant ID
```

Each layer checks one relation and delegates the next relation to the layer
below it. The tools reuse runtime state encoding and script construction, and
the consensus covenant-ID hash function.

## Consensus preimage

Consensus derives a covenant ID from:

- the authorizing input's previous outpoint;
- the number of outputs in the genesis group;
- each output's transaction index, value, script version, and script bytes, in
  group order.

The lowest layer contains no Argent or Silverscript concepts:

```rust
struct ConsensusGenesisProof {
    authorizing_outpoint: TransactionOutpoint,
    claimed_covenant_id: Hash,
    outputs: Vec<IndexedGenesisOutput>,
}

struct IndexedGenesisOutput {
    index: u32,
    value: u64,
    script_public_key: ScriptPublicKey,
}
```

The output indices must be unique and strictly increasing. They do not need to
be contiguous. A proof computes the ID with Kaspa's consensus `covenant_id`
function and checks that it matches `claimed_covenant_id`.

## Silverscript proof

A Silverscript proof explains each P2SH output in the consensus preimage:

```rust
struct SilGenesisProof {
    abis: Vec<SilAbiArtifact>,
    authorizing_outpoint: TransactionOutpoint,
    claimed_covenant_id: Hash,
    outputs: Vec<SilGenesisOutput>,
}

struct SilGenesisOutput {
    index: u32,
    value: u64,
    abi_index: usize,
    contract: String,
    runtime_state: BTreeMap<String, ArtifactValue>,
}
```

ABI units retain caller-supplied order. Each output selects one unit by its
zero-based `abi_index` within the proof.

Each ABI unit is retained unchanged and has its own contract and struct
namespaces. Names may repeat across units.

Verification performs these steps for each output:

1. Find the selected ABI unit and compiled contract.
2. Encode the complete physical runtime state with the ABI.
3. Insert the encoded state into the compiled contract frame.
4. Fold the redeem script into its P2SH script public key.
5. Produce the corresponding `IndexedGenesisOutput`.

The result is a `ConsensusGenesisProof`. The consensus layer then computes the
covenant ID.

The Silverscript proof is self-contained. Cross-app template constants are
already present in the compiled contracts. The proof supplies complete
physical runtime state, including route values.

Every Silverscript proof output names a compiled contract. Use the consensus
layer for groups that contain raw script outputs without a contract-state
claim.

## Argent artifact proof

An Argent proof describes the initial actors and authored states of one
covenant. The app's artifact and dependency artifacts supply the information
needed to derive their physical contract states.

```rust
struct ArgentGenesisProof {
    authorizing_outpoint: TransactionOutpoint,
    claimed_covenant_id: Hash,
    outputs: Vec<ArgentGenesisOutput>,
}

struct ArgentGenesisOutput {
    index: u32,
    value: u64,
    actor: String,
    authored_state: BTreeMap<String, ArtifactValue>,
}
```

One proof describes one genesis group from the primary app. Actor names
resolve only in that app, and outputs may repeat an actor. Dependency actors
cannot join this group. Dependencies supply the checked templates and
interfaces needed by the primary app.

Verification performs these steps:

1. Check the primary artifact and its dependency artifacts.
2. Resolve each actor in the primary app.
3. Validate and encode its authored state.
4. Derive expansion digests and compiler-owned state fields.
5. Produce the corresponding `SilGenesisOutput`.
6. Delegate script construction and covenant-ID calculation to the lower
   layers.

Argent can add state fields that commit to the templates of actors a contract
may later become. The proof derives these fields from the app's artifact,
rather than accepting caller-supplied values. This checks that the genesis
states contain the app's template commitments, in addition to the
Silverscript layer's script and state checks.

## Source verification and dependencies

Source verification connects the proof's artifacts to the app code being
reviewed. It recompiles the selected app and its dependencies, then compares
their artifact IDs with those used by the proof.

Compilation needs the root module and all imported source modules. Foreign
apps used by observes or spawns can contribute templates to the generated
contracts, so their dependencies are part of this check too.

The verifier supplies the source tree separately from the proof package.
Use an Argent compiler version that reproduces the artifacts. That compiler
determines its Silverscript dependency; the generated ABI records the
Silverscript compiler version.

## Portable package

`GenesisProofPackage` carries the data needed to verify one proof at its
selected layer. Its JSON envelope identifies that layer; the proof data is
omitted here:

```json
{
  "schema_version": 1,
  "proof": {
    "kind": "argent",
    "value": {}
  }
}
```

`kind` is `consensus`, `sil`, or `argent`. `value` contains the corresponding
proof data. An Argent package also contains the app and dependency artifacts:

```rust
struct ArgentGenesisPackage {
    primary: Artifact,
    dependencies: Vec<Artifact>,
    proof: ArgentGenesisProof,
}
```

Embedded artifacts remain unchanged.

```rust
let package = GenesisProofPackage::new(ArgentGenesisPackage::new(&bundle, proof));
let json = package.to_json()?;
let loaded = GenesisProofPackage::from_json(&json)?;
loaded.verify_argent(node_covenant_id)?;
```

| Operation | What it does |
| --- | --- |
| `from_json` | Parse JSON and check the package version. |
| `check_consistency` | Check the selected layer's data and its covenant-ID claim. |
| `verify(expected)` | Also compare with the independently supplied covenant ID. |
| `verify_argent(expected)` | Require an Argent-layer package and verify it. |

Use the covenant ID from a node-provided UTXO as `expected`. Unsupported
package versions are rejected by these operations.

State maps use the tagged `ArtifactValue` JSON format. Package output uses
Silverscript's pretty JSON formatter.

## Command-line tools

Compose from Argent source, Argent artifacts, or Silverscript ABI files.
Choose one input mode per command.

### Argent composition

A covenant bootstrap lists the initial actors and their states for one covenant
instance. It also records the authorizing outpoint and output indices and values.
Its required `app` field must match the bundle's primary app. All output actor
names are local to that app.

For example, save this app as `app.ag`:

```rust
state CounterState {
    int count;
}

actor Counter owns CounterState {
    entry hold() emits next: Counter {
        unrestricted(next.value);
        become next <- self;
    }
}

app Counters {
    actor Counter;
}
```

The bootstrap data has this form:

```rust
struct ArgentCovenantBootstrap {
    app: String,
    authorizing_outpoint: TransactionOutpoint,
    outputs: Vec<ArgentGenesisOutput>,
}
```

Save this bootstrap as `genesis.json`. Values are in sompi units; indices are
positions in the full launch transaction:

```json
{
  "app": "Counters",
  "authorizing_outpoint": {
    "transactionId": "6161616161616161616161616161616161616161616161616161616161616161",
    "index": 4
  },
  "outputs": [
    {
      "index": 2,
      "value": 1000,
      "actor": "Counter",
      "authored_state": {
        "count": { "kind": "int", "value": 7 }
      }
    }
  ]
}
```

Replace the example outpoint with the real authorizing input's previous
outpoint.

Compose the package from source:

```text
argentc genesis compose \
  app.ag \
  --app Counters \
  --bootstrap genesis.json \
  --out genesis-proof.json
```

`--app` is optional when the source file declares exactly one app. Imports
supply its source dependencies.

Alternatively, compose from existing artifacts:

```text
argentc build app.ag --app Counters --out build/counters

argentc genesis compose \
  --artifact build/counters/artifact.json \
  --bootstrap genesis.json \
  --out artifact-genesis-proof.json
```

For apps with dependencies, add `--dependency dependency.json` once per
dependency artifact, including transitive dependencies. Their app names and
artifact IDs must match the dependency records.

Composition prints the calculated ID and writes the proof package. `--out`
must name a new file. For verification, obtain the covenant ID independently;
do not use the printed ID as the reference.

### Silverscript composition

Supply ABI files produced by Silverscript and the complete physical runtime
states required by their contracts. The bootstrap uses `SilGenesisOutput`
instead of authored actor states:

```rust
struct SilCovenantBootstrap {
    authorizing_outpoint: TransactionOutpoint,
    outputs: Vec<SilGenesisOutput>,
}
```

For this example, `mint.json` contains a `Mint` contract with an integer
`amount` field, and `ticket.json` contains a `Ticket` contract with an integer
`number` field. Save their bootstrap as `sil-genesis.json`:

```json
{
  "authorizing_outpoint": {
    "transactionId": "6161616161616161616161616161616161616161616161616161616161616161",
    "index": 4
  },
  "outputs": [
    {
      "index": 0,
      "value": 1000,
      "abi_index": 0,
      "contract": "Mint",
      "runtime_state": {
        "amount": { "kind": "int", "value": 7 }
      }
    },
    {
      "index": 2,
      "value": 2000,
      "abi_index": 1,
      "contract": "Ticket",
      "runtime_state": {
        "number": { "kind": "int", "value": 7 }
      }
    }
  ]
}
```

```text
argentc genesis compose \
  --sil-abi mint.json \
  --sil-abi ticket.json \
  --bootstrap sil-genesis.json \
  --out sil-genesis-proof.json
```

`abi_index` follows the order of the `--sil-abi` arguments, starting at zero.

### Verification

Verify directly from source and bootstrap data, without a proof package:

```text
argentc genesis verify \
  --source app.ag \
  --bootstrap genesis.json \
  --covenant-id <node-provided-id>
```

This compiles the app and its dependencies, derives the genesis scripts, and
checks the resulting covenant ID. Use `--app` when the source declares more
than one app. No proof package is written.

Verify any supported package against an independent covenant ID:

```text
argentc genesis verify \
  genesis-proof.json \
  --covenant-id <node-provided-id>
```

This command accepts Argent, Silverscript, and consensus packages. It checks
the package's layer and reports what it checked. Add `--require-argent` to
require the authored-state and generated-field checks of an Argent proof.

Source comparison is an additional Argent check:

```text
argentc genesis verify genesis-proof.json \
  --covenant-id <node-provided-id> \
  --source app.ag \
  --app Counters
```

`--source` recompiles the app and dependencies and compares their artifact IDs
with the package. Without it, verification uses the packaged artifacts.

For a complete example with expanded state and generated route fields, run
this command from the repository root:

```text
cargo run -- genesis compose tests/fixtures/emit/capsule_route_context/app.ag \
  --bootstrap tests/fixtures/genesis_cli/expanded.json \
  --out asset-genesis-proof.json
```

## Bootstrap export API

Both bootstrap types support Serde serialization and deserialization. They
carry the launch data. Call `compose(&bundle)` for an Argent bootstrap or
`compose(abis)` for a Silverscript bootstrap to produce a proof.

An Argent caller can export the authored bootstrap from the same `TxContext`
used to build a launch transaction:

```rust
let bootstrap = ArgentCovenantBootstrap::from_context(
    &bundle, &context, authorizing_input, "launch::asset",
)?;
let json = silverscript_abi::to_pretty_json(&bootstrap)?;
let proof = bootstrap.compose(&bundle)?;
let package = GenesisProofPackage::new(ArgentGenesisPackage::new(&bundle, proof));
```

The exporter selects one group by its authorizing input index and subgroup
name. It preserves the authorizing outpoint, global output indices, values,
and authored states, including expansion preimages. The bootstrap records
the primary app name and local actor names.

Exporting does not build the transaction. The selected group must be nonempty
and contain static authored states for primary-app actors. Raw-script
outputs, foreign-app actors, and deferred state callbacks are rejected.

## Rust API and crate boundaries

`argent-genesis` owns the proof APIs and portable packages. It depends on
`argent-runtime` for artifact bundles, authored-state materialization, and
redeem-script construction. Runtime does not depend on the proof crate.

The proof APIs accept compiled artifacts. A compiler caller obtains the
runtime bundle with `CompiledAppBundle::runtime_bundle()`:

```rust
let bundle = compiled.runtime_bundle()?;
let proof = ArgentGenesisProof::compose(&bundle, authorizing_outpoint, outputs)?;
proof.verify(&bundle, node_covenant_id)?;
```

`ArtifactBundle` checks artifact consistency when each artifact is attached.
`TxBuilder::from_bundle` checks dependency IDs and imported interfaces.
The Argent proof uses `TxBuilder::materialize_actor_state`, which shares the
state conversion used by `TxBuilder::genesis_output`. The Silverscript proof
uses `argent_runtime::materialize_redeem_script` for script construction.

The compiler crate reexports the proof API as `argent::genesis`. Consumers
that do not need compilation can use `argent-genesis` directly. File loading,
source compilation, and node access stay outside both runtime crates.
