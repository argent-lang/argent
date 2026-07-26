# Template receipt decisions

Resolve these questions before this work merges to `master`.

## Actor-type handle presence

Each template receipt has `canonical_template_hash`. It has a separate
`actor_type_handle` only when another state cut is required. When an actor has
no generated context, `argent-rt` uses the canonical template as its actor-type
handle. The artifact omits the duplicate handle.

Decide whether every template receipt must contain `actor_type_handle`,
including when it duplicates the canonical template.

## Canonical template name

The word `canonical` does not say that this hash is for the physical Sil state
cut used inside the defining app. It also does not distinguish this hash from
the actor-type state cut.

Decide whether to rename `canonical_template_hash`. Candidate names include
`sil_template_hash` and `in_app_template_hash`.
