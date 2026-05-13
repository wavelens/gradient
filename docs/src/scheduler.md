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
