# App linking follow-ups

This file lists the immediate work after the first app-linking change. Complete
the artifact checks before the new linking features.

## Verify linked actor receipts

**Area:** Artifact generation and verification, app linking, `argent-rt`, and
runtime tests.

**Context:** An importing artifact records a foreign actor interface and
`actor_type_handle`. The handle includes compiler-generated route context. A
new build of the foreign app can keep the same state field shape but produce a
different handle.

The runtime now compares interface fingerprints. It does not compare the
imported handle with the handle in the attached app artifact. The fingerprint
contains only the actor name, state name, and top-level Sil state fields. It
does not contain nested state layouts, Argent source types, state expansions,
or the actor handle. Artifact verification also does not calculate exported
interface fingerprints again.

**Follow-up:** Define one complete receipt for a linked actor. The receipt must
identify the app, actor, source-state interface, and exact actor handle. The
source-state interface must include all state layouts that the actor state
uses.

Calculate each exported receipt again during artifact verification. Check its
ID, app, actor, state, and actor handle. Reject duplicate receipts. Compare
each imported receipt with a verified export from the attached app artifact.

Add tests for a changed route context, changed nested state, changed source
type, invalid exported fingerprint, missing dependency, and wrong dependency.

## Support cross-app static spawn targets

**Area:** Compiler linking and lowering, artifact metadata, `argent-rt`, and
runtime tests.

**Context:** A static `spawns` target can name an actor in the selected app. A
dynamic target uses an `actor_type<State>` value. A linked foreign actor has an
exported `actor_type_handle`, but it cannot yet be a static spawn target.

Spawn outputs use the same rules as observed outputs with no observed inputs.
Spawn logic also assigns a new covenant group and checks covenant genesis.

**Follow-up:** Process spawn outputs through the same compiler, artifact, and
runtime paths as `observes` outputs. Reuse target resolution, state validation,
and template witnesses. Keep covenant-group assignment and genesis validation
in spawn-specific logic.

For a same-app static target, use the selected app's route plan. For a foreign
static target, use the exported actor handle. Do not add a foreign actor to the
selected app's route graph or route fields.

Record the app and actor for a static target. Record a dynamic target as an
actor-type value. Require and verify the target app artifact at runtime.

Add an end-to-end fixture in which one app spawns an actor from another app.
Give the spawned actor generated route context, so its actor handle differs
from its full Sil template hash. Also test missing and wrong app artifacts,
same-app static spawning, and dynamic spawning.

## Support artifact-backed app dependencies

**Area:** Compiler inputs, project configuration, and bundle builds.

**Context:** An app import now names an Argent source file. The bundle builder
compiles each source app and passes its artifact to dependent apps. A project
can also depend on a published app without its source code.

**Follow-up:** Let project configuration map an app import to source code or an
artifact. Compile a source dependency. Load an artifact dependency without
compilation. Pass the resulting artifact to the importing app in both cases.

Apply all linked actor receipt checks to an artifact dependency.

## Reject runtime app-name collisions

**Area:** App graph validation and `argent-rt` bundle identity.

**Context:** The compiler treats app names as exact strings. The runtime
converts each app name to snake case for its bundle key. Different names such
as `FooBar` and `Foo_Bar` therefore use the same runtime key.

**Follow-up:** Use exact app names as runtime identities, or reject names that
produce the same runtime key. Report the conflict during app graph validation.
