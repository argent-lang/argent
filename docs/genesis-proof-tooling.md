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
    abi: SilAbiArtifact,
    outputs: Vec<SilGenesisOutput>,
}

struct SilGenesisOutput {
    index: u32,
    value: u64,
    contract: String,
    runtime_state: ArtifactValue,
}
```

Verification performs these steps for each output:

1. Find the compiled contract in the ABI.
2. Encode the complete physical runtime state with the ABI.
3. Insert the encoded state into the compiled contract frame.
4. Fold the redeem script into its P2SH script public key.
5. Produce the corresponding `IndexedGenesisOutput`.

The result is a `ConsensusGenesisProof`. The consensus layer then computes the
covenant ID.

The Silverscript proof is self-contained. Cross-app template constants are
already present in the compiled contracts. The proof supplies complete physical
runtime state, including route values.

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
    artifacts: ArgentArtifactBundle,
    outputs: Vec<ArgentGenesisOutput>,
}

struct ArgentGenesisOutput {
    index: u32,
    value: u64,
    actor: ActorPath,
    authored_state: ArtifactValue,
}
```

Verification performs these steps:

1. Check the primary artifact and its dependency artifacts.
2. Resolve each actor in the artifact bundle.
3. Validate and encode its authored state.
4. Derive expansion digests and compiler-owned route state.
5. Produce the corresponding `SilGenesisOutput`.
6. Delegate script construction and covenant-ID calculation to the lower
   layers.

The existing `TxBuilder::genesis_output` path already performs the central
authored-to-physical conversion. The proof implementation should extract or
reuse that path. It must not create a second route-state encoder.

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

The core Rust proof API should accept an already compiled and checked artifact
bundle. Source collection and module loading belong in a higher orchestration
layer:

```rust
fn compose_argent_genesis_proof(
    bundle: &CompiledAppBundle,
    authorizing_outpoint: TransactionOutpoint,
    outputs: &[ArgentGenesisOutput],
) -> Result<ArgentGenesisProof>;
```

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
