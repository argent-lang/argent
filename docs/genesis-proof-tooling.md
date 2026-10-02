# Genesis proof tooling

This document plans tools that explain how an existing covenant ID was
created. A verifier starts with a covenant ID obtained from a node and checks
the launch data against it.

The proof follows the same lowering path as contract construction:

```text
Argent source package + dependency closure
                    |
                    v
          verified Argent artifacts
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

Each layer proves one relation and delegates the next relation to the layer
below it. The implementation must not duplicate state encoding, script
construction, or covenant-ID hashing.

## Consensus preimage

Consensus derives a covenant ID from:

- the authorizing input's previous outpoint;
- the number of outputs in the genesis group;
- each output's transaction index, value, script version, and script bytes, in
  group order.

The output value and transaction index are therefore part of a proof. Initial
actor states and the authorizing outpoint alone are not sufficient.

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
function and compares it with both `claimed_covenant_id` and the independently
trusted ID supplied by the verifier.

The independently trusted ID is normally read from a live UTXO. It is not
taken from the proof package.

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

The proof preserves each original ABI compilation unit. It never merges their
global struct tables. This permits independent ABI units to use the same
contract or struct names and avoids changing the meaning of contract-local
`State` references.

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

This layer checks that the runtime state matches the Silverscript ABI. It does
not prove that a route value expresses the intended Argent route plan. A user
of the lower-level API must verify such application-specific values separately.

The initial format can require every proof output to name a compiled contract.
If genesis groups later need opaque, non-contract outputs, the output recipe
can gain an explicit script-public-key variant. Such an output would contribute
to the covenant ID but would carry no contract-state claim.

## Argent artifact proof

An Argent proof starts from actor names and authored state:

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

The proof owns only its outpoint, claim, and authored outputs. Composition,
lowering, and verification receive the existing `ArtifactBundle` as context.
The bundle borrows artifacts, as `TxBuilder` does. The portable package owns
both the artifacts and this same proof data; it need not reconstruct or copy
the proof to verify it.

One proof describes one genesis group from the bundle's primary app. Actor
names resolve only in that app, and outputs may repeat an actor. Dependency
actors cannot join this group. Dependencies supply the checked templates and
interfaces needed by the primary app.

Verification performs these steps:

1. Check the primary artifact and its dependency artifacts.
2. Resolve each actor in the primary app.
3. Validate and encode its authored state.
4. Derive expansion digests and compiler-owned route state.
5. Produce the corresponding `SilGenesisOutput`.
6. Delegate script construction and covenant-ID calculation to the lower
   layers.

`ArtifactBundle` checks artifact consistency when each artifact is attached.
`TxBuilder::from_bundle` checks the dependency IDs and imported interfaces.
The proof uses the same authored-to-physical state materializer as
`TxBuilder::genesis_output`, then delegates to `SilGenesisProof`. It does not
create a second route-state encoder.

An Argent verifier must derive all compiler-owned values itself. It must not
accept route values from the proof as authoritative. The portable package
contains authored states, not cached physical route values or lower-layer
proofs.

## Source verification and dependencies

Argent source verification needs the complete compilation input, not only the
file that declares the selected app. Imports and observed or spawned foreign
actors can affect generated template constants and route plans.

A portable source package must therefore identify:

- the root module and selected app;
- every source module in the compilation closure;
- linked application artifacts, unless their complete source closures are
  included instead;
- the Argent compiler version.

The selected Argent compiler determines its Silverscript dependency. The
generated Sil ABI records the Silverscript compiler version, so the source
package does not need to declare it separately.

Source verification compiles this closed input and compares the produced
artifact identities with the artifacts used by the Argent proof. Repository
paths or network lookups must not silently supply missing dependencies.

The core Rust proof API accepts a compiled artifact bundle. Source collection
and module loading belong in a higher orchestration layer:

```rust
let bundle = compiled.runtime_bundle()?;
let proof = ArgentGenesisProof::compose(&bundle, authorizing_outpoint, outputs)?;
proof.verify(&bundle, node_covenant_id)?;
```

Compiler callers can obtain this runtime view with
`CompiledAppBundle::runtime_bundle()`. The proof layer itself has no compiler
dependency.

This separation lets callers verify the same genesis through source, Argent
artifacts, or a lower-level Silverscript ABI.

## Portable package

`GenesisProofPackage` contains one selected `GenesisProofLayer`:

- a consensus proof explains only the covenant-ID preimage;
- a Silverscript proof also explains each contract script and physical state;
- an Argent proof also explains each actor and authored state.

The versioned JSON envelope has this shape:

```json
{
  "schema_version": 1,
  "proof": {
    "kind": "argent",
    "value": {}
  }
}
```

`kind` is `consensus`, `sil`, or `argent`. `value` contains the complete data for
that layer. Consensus and Silverscript proofs serialize directly. An Argent
package contains the artifacts and the owned proof:

```rust
struct ArgentGenesisPackage {
    primary: Artifact,
    dependencies: Vec<Artifact>,
    proof: ArgentGenesisProof,
}
```

Embedded artifacts remain unchanged.

An Argent package builds a borrowed `ArtifactBundle` from its owned artifacts
for consistency checking or verification. It uses the stored proof and retains
the published claim. Verification derives the lower layers through the
existing proof APIs.

```rust
let package = GenesisProofPackage::new(ArgentGenesisPackage::new(&bundle, proof));
let json = package.to_json()?;
let loaded = GenesisProofPackage::from_json(&json)?;
loaded.verify_argent(node_covenant_id)?;
```

`from_json` checks the JSON structure and package version, not proof contents.
`check_consistency` checks the selected layer and its claim. `verify` also
compares with the independent covenant ID, but checks only the selected layer.
Callers that require authored-state and route-plan checks use `verify_argent`.
It rejects consensus and Silverscript packages even if they produce the same
ID. All checked operations reject unsupported package versions.

Each higher layer derives and checks the lower layers. None proves that the
artifacts match source code or that the application logic is correct.

State maps use the existing tagged `ArtifactValue` JSON format. Package output
uses Silverscript's pretty JSON formatter. The JSON text is not hashed; the
covenant ID still comes from the consensus output preimage.

A later source package can support reproducible compilation. It requires exact
compiler versions and the complete source and dependency closure. That format
is separate from this artifact package.

## Command-line tools

Compose an Argent package from source and an authored genesis definition:

```text
argentc genesis compose \
  app.ag \
  --app Tickets \
  --definition genesis.json \
  --out genesis-proof.json
```

The source file must declare exactly one app unless `--app` selects it. Imports
supply the complete source dependency closure. Compilation uses a temporary
build directory; it does not write build files into the source tree.

Existing artifacts can supply the same compilation result:

```text
argentc genesis compose \
  --artifact build/launcher/artifact.json \
  --dependency build/launcher/apps/ChildApp/artifact.json \
  --definition genesis.json \
  --out genesis-proof.json
```

Source and artifact inputs are mutually exclusive. Repeat `--dependency` for
all dependency artifacts. The runtime checks their app names and artifact IDs;
the CLI does not accept aliases or fetch missing artifacts.

The definition has no covenant-ID claim:

```rust
struct ArgentGenesisDefinition {
    authorizing_outpoint: TransactionOutpoint,
    outputs: Vec<ArgentGenesisOutput>,
}
```

Output values are KAS values in sompi units. Output indices must be strictly
increasing; the CLI does not sort them. Authored state uses the tagged
`ArtifactValue` JSON format. For example:

```json
{
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

Composition derives the ID, prints it, and writes a self-contained Argent
package with unchanged embedded artifacts. `--out` must name a new file; an
existing file is not overwritten.

Verify any supported package against an independent covenant ID:

```text
argentc genesis verify \
  genesis-proof.json \
  --covenant-id <node-provided-id>
```

This command accepts Argent, Silverscript, and consensus packages. It checks
the selected format and reports which data it checked. A Silverscript package
needs no Argent app or metadata; its independent ABI units remain separate.
Use `--require-argent` when authored-state and route-plan checks are required.

Source comparison is an additional Argent check:

```text
argentc genesis verify genesis-proof.json \
  --covenant-id <node-provided-id> \
  --source app.ag \
  --app Tickets
```

It verifies the stored claim, recompiles the source and dependencies, and
compares the primary and dependency artifact IDs with the package. It never
replaces the claim with a new ID. Without `--source`, the command states that
source correspondence was not checked. Neither mode proves application logic
correct.

CLI composition from Silverscript source or ABI files, and comparison with
Silverscript source, remain follow-up work. The Rust API already supports
Silverscript composition. Portable source collection also remains separate.

The CLI tests use a complete definition with expanded state and route fields:

```text
cargo run -- genesis compose tests/fixtures/emit/capsule_route_context/app.ag \
  --definition tests/fixtures/genesis_cli/expanded.json \
  --out asset-genesis-proof.json
```

Node access remains outside the first implementation. The caller supplies the
covenant ID obtained from the selected UTXO.

## Crate boundaries

`argent-genesis` owns the proof APIs and portable packages. It depends on
`argent-runtime` for artifact bundles, authored-state materialization, and
redeem-script construction. Runtime does not depend on the proof crate.

The compiler crate reexports the proof API as `argent::genesis`. Consumers
that do not need compilation can use `argent-genesis` directly. File loading,
source compilation, and node access stay outside both runtime crates.

## Implementation sequence

1. Add the consensus proof types and covenant-ID verification.
2. Add the self-contained Silverscript proof and lower it to a consensus proof.
3. Add the Argent artifact proof and lower authored actor states to the
   Silverscript proof.
4. Add JSON package encoding, then command-line composition and verification.
5. Add a source-package format with the complete dependency closure.

Every step must produce and verify the lower-layer representation. Tests must
cover repeated actor types, non-contiguous output indices, changed output
order, changed values, changed states, changed contracts, route-bearing state,
expanded state, dependency mismatches, and a node-provided covenant-ID
mismatch.

## First implementation leg

The consensus layer lives in `crates/argent-genesis/src/preimage.rs`.

It should:

- add `ConsensusGenesisProof` and `IndexedGenesisOutput` in a focused module;
- validate a nonempty, strictly ordered output list;
- call Kaspa's consensus `covenant_id` function directly;
- expose separate operations to compute the ID and compare it with an external
  expected ID;
- return precise errors for an empty proof, non-increasing output indices, a
  mismatched package claim, and a mismatched external ID;
- avoid serialization, Argent artifacts, Silverscript ABI data, source loading,
  and command-line work.

Tests should compare this calculation with
`Transaction::populate_genesis_covenants`. They should also change the
authorizing outpoint, an output index, value, script version, and script bytes,
and prove that each change affects verification.

This leg establishes the consensus meaning of every later proof without making
any decision about the portable package format.

## Second implementation leg

The Silverscript layer lives in `crates/argent-genesis/src/sil.rs`.

It should:

- preserve independent Sil ABI compilation units in a vector;
- check each ABI without merging its contracts or structs with another ABI;
- encode complete physical runtime state and insert it into the checked
  contract frame;
- fold each redeem script into P2SH and produce a
  `ConsensusGenesisProof`;
- share contract-frame materialization with `TxBuilder`;
- expose composition, internal consistency, and external-ID verification;
- avoid serialization, Argent-authored state, source loading, and command-line
  work.

Tests must include two independent ABI units with the same contract and struct
names but different definitions. They must also cover an invalid ABI, unknown
ABI and contract references, malformed runtime state, state mutation, and an
external covenant-ID mismatch.

## Third implementation leg

The Argent layer lives in `crates/argent-genesis/src/ag.rs`.

It should:

- use the existing runtime `ArtifactBundle` as context and check its dependency
  closure;
- keep every genesis actor within the primary app;
- accept authored state maps, including nested expansion preimages;
- derive generated route fields and expansion digests through the existing
  `TxBuilder` state materializer;
- produce one `SilGenesisProof` using the primary app's embedded ABI;
- retain the published covenant-ID claim when lowering, and compare it with
  an independently supplied ID during verification;
- keep proof data separate from the bundle; leave package ownership,
  serialization, source loading, and command-line work for later legs.

Compiled-app tests should compare the proof with an executed genesis
transaction and with `TxBuilder::genesis_output`. They must cover repeated
actors, non-contiguous output indices, generated route context, expansion
digests, changed authored states and actors, malformed states, rejected
caller-supplied route fields, missing or mismatched dependencies, and a
foreign actor in the genesis group.

## Fourth implementation leg

This leg has two review units:

- **4a: Package API.** Add Serde support to the owned consensus and Silverscript
  proofs and to the Argent proof data. Add an owned Argent package and a
  versioned envelope with an explicit verification layer. Keep parsing separate
  from consistency checking and external-ID verification. Test JSON
  round-trips, unchanged embedded artifacts, retained claims, changed proof
  data, missing or mismatched dependencies, and unsupported versions.
- **4b: Command-line tools.** Compose and verify these packages through
  `argentc`. Keep file loading and compilation outside `argent-genesis` and
  `argent-runtime`.
  Require an independent covenant ID for verification. Leave source-package
  collection and node access for later work.
