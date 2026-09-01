# In-app actor template identity

This document defines how Argent keeps actor identities distinct inside one
compiled app. It expands the corresponding invariant in the
[security invariants index](README.md).

## Closed app domain

A covenant ID defines a closed app domain. Inputs carrying that ID are assumed
to descend from outputs authorized by the app. Generated template readers do
not identify contracts from a global set. They distinguish actors within that
closed app.

This invariant does not require global template uniqueness between unrelated
apps. It protects actor identity inside one compiled app. Argent does not rely
on the app author, whether unaware or malicious, to preserve that identity.

## Operational actor identity

Argent implements template-authenticated cross-actor state checks through
Silverscript's
[template builtins](https://github.com/kaspanet/silverscript/blob/master/silverscript-lang/std/builtins.sil).
A consume uses `readInputStateWithTemplate` to authenticate an input template
before it reads the physical state. An emit uses
`validateOutputStateWithTemplate` to validate a successor state against its
target template.[^single-actor]

Both builtins use a template hash and the physical state layout chosen at
compile time. Silverscript computes the fixed encoded state length from the
flattened `State` layout and rejects a template operation if this width cannot
be determined at compile time. The hash authenticates the fixed code around
the state. The state length locates the state boundary and the suffix.
Together, `(template hash, physical state length)` is the actor identity used
by generated contracts inside one app.

[^single-actor]: A single-actor app can use same-template shortcuts such as
    `readInputState` and `validateOutputState` because it does not need to
    distinguish between in-app actor templates.

## Template frame

For a compiled actor `A`, let:

```text
P_A = fixed template prefix
L_A = encoded physical state length
Q_A = fixed template suffix
```

The actor's *template frame* is the fixed prefix and suffix around its
fixed-length physical-state span:

```text
F_A = [ P_A ][ physical state: L_A bytes ][ Q_A ]
```

Its template hash is:

```text
H_A = blake3(i64le(P_A.length) || P_A || i64le(Q_A.length) || Q_A)
```

A complete redeem script `R` matches this frame when it can be written as:

```text
R = P_A || S || Q_A
S.length = L_A
```

The bytes in `S` vary between covenant instances. The prefix and suffix remain
fixed for the compiled actor.

## Required partition

The operational identity `(H_A, L_A)` makes each actor frame well defined. It
does not make actor detection unique: one complete redeem script can still
match two different frames. Within the closed app domain, the frames must
therefore partition the valid redeem scripts. Each valid script must belong to
exactly one actor. For any two distinct in-app actors `A` and `B`, their
matching script sets must be disjoint:

```text
scripts(F_A) intersect scripts(F_B) = empty
```

## Boundary ambiguity

Identical generated code and state lengths are the simplest violation:

```text
actor A: [ prefix ][ state: L bytes ][ suffix ]
actor B: [ prefix ][ state: L bytes ][ suffix ]
```

The actors have the same identity pair and accept the same scripts.

Different identities can also overlap when their state boundaries shift in
opposite directions:

```text
actor A: [ 1 2 3   ][ state: L bytes ][ 7 8 9 10 ]
actor B: [ 1 2 3 4 ][ state: L bytes ][   8 9 10 ]
```

For `L = 3`, the same complete script has these two interpretations:

```text
actor A: [ 1 2 3   ][ 4 a b ][ 7 8 9 10 ]
actor B: [ 1 2 3 4 ][   a b 7 ][ 8 9 10 ]
```

Actor A treats `4` as state. Actor B treats `7` as state. Both template hashes
and state lengths are well defined, but actor detection is not unique.

## Conservative ambiguity rule

Argent uses a conservative ambiguity rule. Two actors conflict when:

```text
their complete script lengths are equal
and either prefix is a prefix of the other prefix
and either suffix is a suffix of the other suffix
```

More formally, for complete script length:

```text
T_A = P_A.length + L_A + Q_A.length
```

Argent rejects `A` and `B` when:

```text
T_A = T_B
and (P_A starts with P_B or P_B starts with P_A)
and (Q_A ends with Q_B or Q_B ends with Q_A)
```

### Safety proof

Assume that one script `R` matches both `F_A` and `F_B`. Then:

```text
T_A = R.length = T_B
```

`P_A` and `P_B` are both prefixes of `R`, so the shorter is a prefix of the
longer. Likewise, `Q_A` and `Q_B` are both suffixes of `R`, so the shorter is a
suffix of the longer. The rule predicate must therefore be true for every pair
of overlapping frames.

Argent checks every distinct actor pair and accepts the app only when the
predicate is false for every pair. Therefore:

```text
scripts(F_A) intersect scripts(F_B) = empty
```

The accepted frame sets are pairwise disjoint.

### Conservative false positives

The full exact algorithm would perform one additional step. It would align the
complete frames and compare every position fixed by both actors. It would
reject only when all shared fixed positions agree. For example, these frames
satisfy the conservative rule but cannot match the same script:

```text
position:   0  1  2  3  4  5

actor A:   [1, 2, 3, 4, ?, 9]
actor B:   [1, ?, 6, 7, 8, 9]
                  ^  ^
             conflicting bytes
```

The conservative rule rejects this pair without checking the conflicting
bytes. This false positive requires equal complete lengths, comparable outer
bytes, and fixed regions from opposite actors that cross through the state
span. There is no useful reason for two actors in one app to have this unusual
relationship. Rejecting it keeps the rule short and makes independent compiler
and artifact implementations easier to audit.

## Actor-type handle corollary

An external observer only needs a fixed template around the fixed state shape
named by `actor_type<state_type>`. It does not need the actor's compiler-owned
route context. Argent fixes that context in an extended template prefix and
leaves `state_type` open. The resulting actor-type handle lets Argent pass this
external identity as a value. See
[source-state actor handles](../icc-semantics.md#source-state-actor-handles) and
the compiler's [state representations](../compiler-design.md#state-representations).

Actor-type handles do not need a second pairwise ambiguity check. A handle
restricts the original actor frame: every script that matches the handle also
matches the actor. Pairwise-disjoint actor frames can therefore have only
pairwise-disjoint handle frames.

More formally, let `C_A` be the fixed leading state context moved into actor
`A`'s handle. The handle frame `G_A` is:

```text
P'_A = P_A || C_A
L'_A = L_A - C_A.length
Q'_A = Q_A

G_A = [ P'_A ][ open state: L'_A bytes ][ Q'_A ]
```

The complete length does not change. A script that matches `G_A` can treat
`C_A` followed by the open state as the original `L_A`-byte physical state.
Therefore:

```text
scripts(G_A) is a subset of scripts(F_A)
```

For two accepted actors `A` and `B`:

```text
scripts(G_A) intersect scripts(G_B)
    is a subset of scripts(F_A) intersect scripts(F_B)
    = empty
```

Thus, accepted in-app actor-type handle frames are pairwise disjoint. This
reduction requires `C_A` to encode exactly the leading physical-state fields
and to occupy their full encoded width. The handle state type covers all
remaining fields. Compiler generation and artifact verification enforce these
conditions.

## Enforcement

The compiler applies the rule only after:

- all in-app actor contracts are compiled;
- compiler-owned route fields are final;
- every physical state span is final.

For each actor, the compiler obtains `P_A`, `L_A`, and `Q_A` from the compiled
bytecode and its Silverscript state span. It compares every distinct pair of
actors in the selected app. An ambiguous pair stops artifact generation.

Artifact verification repeats the same comparison over the embedded
Silverscript contracts. The artifact is not trusted to claim that its actor
identities are disjoint.

An error should name both actors and report their prefix, state, suffix, and
complete lengths. It should state that the conservative frame rule found an
ambiguity. It should not claim that a concrete shared script exists when the
match can be a conservative false positive.

## Separation by construction

Rejecting ambiguous frames is the minimum safe behavior. A possible alternative
is semantically inert generated code outside the physical state span, using a
unique app-local actor index as a discriminator.

Any separation method must be verified against the final compiled frames. If
ambiguity remains, the compiler rejects the app.
