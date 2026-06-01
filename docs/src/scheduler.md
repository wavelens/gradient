# Scheduler

The Gradient scheduler coordinates build dispatch across connected workers.
This page covers the cross-cache deduplication feature. For a general
overview of the scheduler architecture see
[Architecture](development/architecture.md).

### Cross-cache deduplication

When a new build is created for `/nix/store/<hash>-foo.drv` and another
organisation already has an in-flight build for the same path, the new build
can be linked to it as a follower instead of being scheduled separately.
The link is admissible whenever the follower's organisation can substitute
from the leader's writes through the cache graph:

- The leader's organisation has `organization_cache` mode `ReadWrite` or
  `WriteOnly` on some cache `C_w`.
- `C_w` is in the upstream closure (over `cache_upstream`) of one of the
  follower organisation's `ReadWrite`/`ReadOnly` caches.

The closure walk follows only internal `cache_upstream.upstream_cache`
edges; external (URL-based) upstreams do not host Gradient builds and are
excluded.

#### Leader selection

`find_active_leaders` first looks for an in-flight candidate in the same
organisation. Only when none exists does it run the cross-org pass.
Cross-org candidates are filtered to `external_cached = false` and ordered
by status (`Building` > `Queued` > `Created`) then oldest `created_at`.

#### Artefact propagation

Same-org followers share the leader's `derivation` row, so its
`derivation_output` and `build_product` children are visible automatically.
Cross-org followers have these rows mirrored onto their own `derivation`
when the leader completes (see `scheduler::build::propagate_to_followers`
and the pure helper `build_cross_org_artefact_rows`).

#### Access

Read-only build endpoints (`GET /builds/{id}`, `/log`, `/downloads`,
`/graph`) accept requests from members of any organisation that holds a
follower row pointing at the targeted leader.

#### Leader abort

On leader abort, only same-org followers are eligible for promotion to the
new leader. Cross-org followers are made independent (`via` cleared) so the
next dispatch cycle picks them up on their own.

### Log substitution from upstream caches

When a derivation's outputs are pulled from an upstream cache rather than
built locally, Gradient also tries to retrieve the corresponding build log
from that upstream's `/log/{drv}` endpoint (the same one the Gradient cache
exposes). If the upstream serves the log, it is stored under the same build
record so the build's log tab shows it just like a locally-built one. If no
upstream serves the log, the build is recorded without one.

## Adaptive fetch/eval split

When the scheduler detects an idle dedicated eval-only worker — determined by
checking whether any connected worker is eval-only (fetch capability absent)
and has no currently assigned job — it splits a flake evaluation into two
sequential jobs instead of dispatching the usual bundled fetch+eval job. The
split is a heuristic: if no idle eval-only worker exists at dispatch time, the
original bundled job is issued unchanged.

The first job (`FetchFlake` task only, `FlakeSource::Repository`) is routed
exclusively to a fetch-capable worker; once it completes, the scheduler reads
`evaluation.flake_source` from the finished job and immediately enqueues a
cached-eval follow-up (`EvaluateFlake` + `EvaluateDerivations` tasks,
`FlakeSource::Cached`) that any eval worker can run. The eval worker
substitutes the cached source NAR from the gradient binary cache into its
local store before evaluating, since a `path:` flakeref must point to a
locally-present path. The `ReserveFetchWorkersRule` scoring policy applies a
penalty when a fetch-capable worker is offered a cached-eval job, steering
those workers toward fetch work; it is a soft steer rather than a ban, so a
fetch worker still accepts cached-eval jobs when no other candidate is
available.
