# Covenant state provenance

This document explains when one covenant can treat another covenant input as a
previously authorized state. It expands the corresponding invariant in the
[security invariants index](README.md).

## Why provenance comes first

A typed state reader is not intended to give arbitrary UTXO bytes semantic
meaning. Before an application reads another covenant's state, it must have a
reason to trust the state producer or its history. Without that trust, the
decoded values are still attacker-controlled.

A useful analogy is a contract interface in a VM. The interface defines the
shape of a call and its result, but does not prove that an arbitrary contract
implements the intended logic. The caller separately authenticates the
contract instance or implementation through a stored identity, registry,
whitelist, signature, or another protocol rule. The interface remains useful
because the trusted implementation can be created after the caller.

A covenant interface separates the same concerns:

- a state schema defines the interface;
- a covenant program template identifies an implementation;
- a covenant ID identifies a persistent instance and its lineage;
- application policy decides why that implementation and lineage are trusted.

This separation supports late-bound composition. A covenant can read another
covenant created after it was compiled, provided that it obtains an
authenticated program identity and covenant ID which satisfy its policy. The
state shape enables the interaction; the authenticated provenance gives the
state meaning.

The core reduction is:

```text
meaningful typed state
    requires authenticated provenance

trusted covenant provenance
    implies state created by validated, canonical writes
    implies canonical field framing
```

Without provenance, validating field framing proves only that arbitrary data
is well formed. It does not make the data an authoritative protocol state.
Likewise, an input index identifies where to read; it does not establish why
the selected state should be trusted.

## Closed covenant lineage

A covenant ID identifies one closed covenant lineage. After genesis, consensus
permits an output to keep that ID only when an input with the same ID authorizes
it. [KIP-20](https://github.com/kaspanet/kips/blob/master/kip-0020.md) defines
this continuation rule and commits the initial genesis outputs to a unique
authorizing outpoint.

The consuming application must establish that the selected genesis outputs are
the intended initial state. Under that assumption, every later UTXO with the
same covenant ID must descend from an earlier member of that lineage.

The covenant ID identifies the application instance, not one program within
the application. A reader separately authenticates the expected covenant
program template and physical state layout. If the application contains more
than one program, those program identities must be unambiguous.

## Two separate proofs

A typed cross-input state relies on two independent facts:

1. **Lineage provenance.** The input carries the covenant ID selected by the
   entry. Consensus proves membership in a lineage whose trusted genesis and
   validated continuations preserve canonical state writes.
2. **Program identity.** The reader authenticates the input's program template
   before it decodes the physical state. This proves which program and state
   layout the input uses.

Lineage membership alone does not identify a program. Template authentication
alone does not prove that the input belongs to the intended covenant instance.
A typed read needs both facts.

For an external input, Silverscript can use
`readInputStateWithTemplate`. This builtin verifies the input's P2SH script and
template hash before it reads the state fields. It does not independently
validate the data-push opcode before each field. A restricted same-template
case can use `readInputState`; the
[single-actor rule](README.md#single-actor-direct-input-state-reads) makes the
expected template implicit.

The compiler representation of these checks is described under the
[authenticated input boundary](../compiler-design.md#authenticated-input-boundary).

## State provenance by induction

For a sound covenant lineage, each authorizing program validates the complete
set of continuation outputs it authorizes. It checks their templates and states
according to the application protocol. Consensus then records each output
under the covenant ID of its authorizing input.

This gives a short induction over the covenant lineage:

- **Base case:** the trusted genesis definition commits to the intended initial
  outputs.
- **Continuation step:** assume an input is an authorized application output.
  Its covenant program executes when it is spent. The program accepts only its
  authorized outputs and validates their target templates and states. KIP-20
  permits those outputs to continue the covenant ID because that input
  authorizes them.
- **Result:** every later covenant input descends from an output accepted by a
  prior app transition.

```text
trusted genesis output
        |
        | covenant program authorizes and validates the next output
        v
continuation output with the same covenant ID
        |
        | repeated app transitions
        v
current cross-input reference
        |
        | authenticate program template, then decode state
        v
typed state projection
```

The current reader does not need the parent transaction or a proof of the full
history. Consensus checked each continuation when it was created, and each
authorizing contract checked the corresponding state write.

Here, an *authorized state* means a state value accepted by the preceding
covenant contract. It does not mean that the value satisfies an application
rule which the developer did not encode.

## Canonical state framing

Silverscript encodes each physical state field as a data push in the redeem
script. The input-state builtins know the field types and their encoded widths.
They use those widths to calculate each field's byte range. They are not general
parsers for a redeem script with attacker-controlled field framing.

In particular, `readInputState` and `readInputStateWithTemplate` do not validate
the push opcode before every field they read. The template-aware form still
proves that the claimed redeem script matches the input's P2SH script and that
its fixed prefix and suffix match the expected template. Those checks do not,
by themselves, prove canonical framing inside the variable state span.

Canonical framing instead follows from state provenance:

- the trusted genesis state uses the compiler's state encoding;
- `validateOutputState*` constructs every changed successor from typed fields
  and writes their canonical data pushes;
- an exact self-continuation preserves the complete existing script.

The induction therefore preserves both the meaning and the framing of the
state fields. A reader can use fixed offsets without repeating the push checks
at every later transition.

This reasoning does not apply to an arbitrary covenant ID or to an
attacker-created genesis that merely uses a compatible template. For example,
a DEX can establish provenance by binding a token input to a whitelisted
covenant ID. An application which intentionally inspects arbitrary scripts
needs explicit byte-level validation instead of an Argent typed-state read.

## Argent-managed input references

The invariant applies to input references created from Argent declarations:

- `self`, which is the active covenant input;
- `consumes` handles, which select inputs from the active covenant lineage;
- `observes` inputs, which select inputs from the declared foreign covenant
  lineage.

Argent also requires in-app actor templates to be unambiguous, as defined by
[In-app actor template identity](template-frame-identity.md).

Argent first establishes the input location, covenant lineage, and expected
actor template. It then permits direct authored-field projection or complete
authored-state reconstruction through `state(ref)`. Compiler-owned route
fields are not part of the authored value.

For an observed covenant, the observing app must obtain the expected covenant
ID and actor identity from a trusted source. Authentication proves that the
selected input matches those values; it cannot decide whether the application
chose the correct foreign covenant. If that covenant was not produced by
Argent, its implementation must enforce equivalent continuation checks.

## Output responsibility

The provenance argument depends on generated contracts checking every output
that they authorize. Argent therefore enforces the declared authorized-output
count and order, validates successor templates and physical states, and checks
the covenant output shape. A missing output check could create a same-ID UTXO
which did not pass the intended app transition.

Spawned actors form the base of a new lineage rather than continuing an
existing one. Their generated checks reconstruct the KIP-20 genesis covenant
ID from the complete declared output group. The genesis output set is then the
base case for later state provenance.

## Trust boundary

This invariant assumes:

- consensus correctly enforces KIP-20 covenant IDs and transaction scripts;
- the intended covenant ID and canonically encoded genesis outputs are obtained
  from a trusted source;
- the Argent and Silverscript compilers generate the documented input and
  output checks;
- any compatible foreign covenant enforces its declared continuation checks;
- the expected artifact, linked app, and actor handle identities are trusted;
- the relevant hash and P2SH commitments remain secure.

The guarantee does not cover arbitrary low-level Silverscript calls written by
the developer. In particular, a direct `readInputState` call outside Argent's
planned input-reference lowering is developer-managed. It also does not make a
foreign covenant ID supplied by an untrusted caller authoritative.
