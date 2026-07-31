# Body lowering notes

Small implementation hints to revisit during the current compiler work.

- Give helper `fn` bodies token lowering for `cov_id` locals and `.co_spent()`, using parameter, field, and local types.
- Accept Sil ternaries, bitwise XOR, and single-quoted strings in the shared lexer.
- Decide whether unbraced `if` bodies pass through or are rejected by `EntryBody`; remove the policy from emission.
- Unify `EntryRoute` to `RouteCall` conversion when it has a clean dependency home.
- Match route-target delimiter kinds so malformed expressions receive structural errors.
