# Body lowering notes

Small implementation hints to revisit during the current compiler work.

- Give helper `fn` bodies token lowering for `cov_id` locals and `.co_spent()`, using parameter, field, and local types.
- Define actor-function `self` semantics. Actor functions currently access contract fields by bare Sil names and do not receive entry `self.*` lowering; if that syntax is added, reserve `self` as a function binding at the same time.
- Accept Sil ternaries, bitwise XOR, and single-quoted strings in the shared lexer.
- Reject locals that shadow fixed Argent references before applying entry-wide rewrites.
- Put entry-root bindings in one namespace. Reject a local such as `Turn next` when `next` is already an emit handle; contextual resolution of `next.value` and `Pong(next)` currently makes this legal but visually ambiguous.
- Attach body-local selector metadata to parsed binding identities instead of keeping the analyzed selector catalog entry-wide and keyed by name.
- Decide whether unbraced `if` bodies pass through or are rejected by `EntryBody`; remove the policy from emission.
- Put statement terminator policy in `EntryBody`; plain statements and `become` currently enforce it differently.
- Reject or define empty `become {}`; it currently becomes a no-op for `emits none`.
- Retain each entry body's file offset so body diagnostics use source-file locations.
- Match route-target delimiter kinds so malformed expressions receive structural errors.
