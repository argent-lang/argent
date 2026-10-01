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
    bundle: ArtifactBundle,
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

The runtime API uses the existing `ArtifactBundle`. It borrows the artifacts,
as `TxBuilder` does. An owned portable package is a later layer.

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
accept route values from the proof as authoritative. If the serialized package
also contains the derived Sil proof, verification regenerates it and requires
an exact match.

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
proof.verify(node_covenant_id)?;
```

Compiler callers can obtain this runtime view with
`CompiledAppBundle::runtime_bundle()`. The proof layer itself has no compiler
dependency.

This separation lets callers verify the same genesis through source, Argent
artifacts, or a lower-level Silverscript ABI.

## Portable package

The portable package should contain the highest verification layer that the
publisher wants to support:

- a consensus proof explains only the covenant-ID preimage;
- a Silverscript proof also explains each contract script and physical state;
- an Argent proof also explains each actor and authored state;
- an Argent source package also supports reproducible compilation.

A package may cache lower-layer data for inspection. Each verifier must
regenerate that data from the higher layer and compare it exactly. Cached data
is evidence, not authority.

The source package is useful for human review, but verified artifacts are the
stable execution evidence. Source verification also depends on exact compiler
versions and the complete dependency closure.

## Command-line shape

The command-line interface can expose the same layers:

```text
argentc genesis compose \
  app.ag \
  --app Tickets \
  --definition genesis.json \
  --out genesis-proof.json

argentc genesis verify \
  app.ag \
  --app Tickets \
  --proof genesis-proof.json \
  --covenant-id <node-provided-id>
```

Artifact and Silverscript modes can start at lower layers:

```text
argentc genesis verify \
  --artifact-bundle build/tickets \
  --proof genesis-proof.json \
  --covenant-id <node-provided-id>

argentc genesis verify \
  --sil-abi contracts.json \
  --proof genesis-proof.json \
  --covenant-id <node-provided-id>
```

Node access remains outside the first implementation. The caller supplies the
covenant ID obtained from the selected UTXO.

## Implementation sequence

1. Add the consensus proof types and covenant-ID verification.
2. Add the self-contained Silverscript proof and lower it to a consensus proof.
3. Add the Argent artifact proof and lower authored actor states to the
   Silverscript proof.
4. Add JSON package encoding and command-line composition and verification.
5. Add a source-package format with the complete dependency closure.

Every step must produce and verify the lower-layer representation. Tests must
cover repeated actor types, non-contiguous output indices, changed output
order, changed values, changed states, changed contracts, route-bearing state,
expanded state, dependency mismatches, and a node-provided covenant-ID
mismatch.

## First implementation leg

The first commit adds only the consensus layer to `argent-runtime`.

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

The second commit adds `genesis_proof/sil.rs`.

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

The third commit adds `genesis_proof/ag.rs`.

It should:

- use the existing runtime `ArtifactBundle` and check its dependency closure;
- keep every genesis actor within the primary app;
- accept authored state maps, including nested expansion preimages;
- derive generated route fields and expansion digests through the existing
  `TxBuilder` state materializer;
- produce one `SilGenesisProof` using the primary app's embedded ABI;
- retain the published covenant-ID claim when lowering, and compare it with
  an independently supplied ID during verification;
- leave portable package ownership, serialization, source loading, and
  command-line work for later legs.

Compiled-app tests should compare the proof with an executed genesis
transaction and with `TxBuilder::genesis_output`. They must cover repeated
actors, non-contiguous output indices, generated route context, expansion
digests, changed authored states and actors, malformed states, rejected
caller-supplied route fields, missing or mismatched dependencies, and a
foreign actor in the genesis group.
