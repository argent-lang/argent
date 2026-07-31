# Body lowering notes

Small implementation hints to revisit during the current compiler work.

- Give helper `fn` bodies token lowering for `cov_id` locals and `.co_spent()`, using parameter, field, and local types.
- Accept Sil ternaries, bitwise XOR, and single-quoted strings in the shared lexer.
- Track `for`, tuple, and destructuring binders in type-sensitive body lowering.
- Reject locals that shadow fixed Argent references before applying entry-wide rewrites.
- Decide whether unbraced `if` bodies pass through or are rejected by `EntryBody`; remove the policy from emission.
- Put statement terminator policy in `EntryBody`; plain statements and `become` currently enforce it differently.
- Reject or define empty `become {}`; it currently becomes a no-op for `emits none`.
- Retain each entry body's file offset so body diagnostics use source-file locations.
- Unify `EntryRoute` to `RouteCall` conversion when it has a clean dependency home.
- Match route-target delimiter kinds so malformed expressions receive structural errors.
- Replace BodyLowerer’s cloned type maps with a scoped binding table that carries optional source and lowered types plus scope-local materialization state.