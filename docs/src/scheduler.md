# Scheduler

The Gradient scheduler coordinates build dispatch across connected workers.
This page covers how builds are shared across evaluations and organisations.
For a general overview of the scheduler architecture see
[Architecture](development/architecture.md).

### Shared build anchors

A derivation is built exactly once, globally. Build state lives on a
`derivation_build` anchor keyed 1:1 to the content-addressed `derivation`
(UNIQUE on `derivation`, so the database itself enforces build-once). Every
evaluation that needs a derivation gets a per-eval `build_job` linking it to
that anchor; each execution attempt and its log live on `build_attempt` under
the anchor.

When two evaluations - in the same or different organisations - need the same
derivation, they share the one anchor: whichever is dispatched first builds
it, and the others observe the result the moment the anchor reaches a
terminal-success status. There is no leader/follower row and no `via` link;
sharing is implicit in the global derivation graph.

#### Promotion

An anchor becomes `Queued` (buildable) the moment all of its dependency
anchors are terminal-success (`Completed`/`Substituted`), independent of any
single evaluation's completion (see `gradient_db::promotion`). This decoupling
is what keeps builds from getting stuck behind a never-completing evaluation. A
failed dependency cascades `DependencyFailed` over the global
`derivation_dependency` graph.

Promotion and dispatch are gated on reachability: an anchor is queued and
dispatched only while some `build_job` references its derivation. Anchors are
seeded for every derivation, so without the gate promotion would queue
derivations no surviving evaluation needs, leaving the dispatcher unable to
attribute the build to a driving evaluation.

They are also gated on `derivation_build.edges_complete`. Anchors are created
per-batch as the evaluation streams, but `derivation_dependency` edges are
deferred and flushed in one pass at the eval's completion. An anchor with no
edges is therefore ambiguous: a genuine leaf, or a node whose edges are not
written yet. A failed, aborted, or restart-interrupted eval leaves its anchors
edge-less; promoting them as if they were dependency-free dispatches builds
without their inputs (`InputsUnavailable`). So an anchor is promotable only once
the eval that owns it flushes its edges and calls
`mark_edges_complete_for_eval`, which sets `edges_complete` for every anchor that
eval's `build_job`s reference. The flag is monotonic and content-addressed:
edges never change once written, so a later requeue keeps the anchor promotable
without re-evaluation. `promote_ready`, `promote_dependents`, and the dispatch
readiness query all require it.

Because the anchor is global and build-once, a new evaluation is treated as a
fresh build intent: `resolve_anchors` re-queues anchors a previous eval left
terminal-failed, and the substitute-miss budget is scoped per evaluation. A
permanent failure (or an exhausted substitute budget) therefore does not poison
every later evaluation that needs the derivation - the world (upstream cache,
network) may have changed since it failed.

#### Upstream substitutability

A derivation is just another build that can be substituted when its output is
available on a cache, exactly like any other - fixed-output derivations are not
special-cased. At eval time `resolve_anchors` runs an org-scoped lookup
(`compute_upstream_substitutable`): for every derivation not already in the
gradient cache it probes each output's `.narinfo` across the org's configured
upstream caches. A derivation is marked substitutable only when *every* one of
its outputs is cached somewhere (the gradient cache or an upstream); otherwise it
is built. The resolved upstream NAR URL plus narinfo metadata is persisted once
onto `derivation_output` (`external_url`, `nar_hash`, `file_size`,
`references_list`, `deriver`), so the narinfo lookup runs only once. Substitutable
anchors dispatch through the existing `external_cached` path. The dispatch carries
the derivation's output `(name, store_path)` pairs in the `BuildTask` so the worker
fetches the outputs directly and never touches the `.drv`: a substitution needs
only the output NAR plus its runtime closure, never the `.drv`'s build-time
`input_sources` (binary caches do not serve those, so importing the `.drv` would
fail with a spurious `SubstituteUnavailable`). The worker reads each output's
persisted URL via `CacheQuery`, downloads the NAR directly from the upstream,
recompresses it, and pushes it into the gradient cache (`use_substitutes` stays
off in the daemon - substitution always goes through gradient, never the worker's
own nix config). Existing build-once anchors a prior eval left not-yet-succeeded
are flipped substitutable when an upstream is newly found, so a previously-failed
fetcher substitutes instead of rebuilding.

The eval closure walk prunes the same way. As the worker walks the graph it
asks the server which dependency derivations it already knows
(`QueryKnownDerivations`); the server prunes a subtree only when the derivation's
complete output set is fetchable without building it - every output is in our
cache (`is_cached`, or its hash recorded in `cached_path`) or on a known upstream
(`external_url`). This mirrors the eval-time substitutability decision, so the
worker skips re-walking the whole upstream closure (e.g. nixpkgs) on every
evaluation instead of descending into subtrees it will never build.

#### Access and GC

Read-only build endpoints (`GET /builds/{id}`, `/log`, `/downloads`,
`/graph`) accept requests from members of any organisation whose evaluation
references the derivation (a `build_job` exists for it in one of that org's
evaluations). The same reachability refcounts the anchor for garbage
collection: a derivation with no surviving `build_job` is collected once past
its grace period.

### Log substitution from upstream caches

When a derivation's outputs are pulled from an upstream cache rather than
built locally, Gradient also tries to retrieve the corresponding build log
from that upstream's `/log/{drv}` endpoint (the same one the Gradient cache
exposes). If the upstream serves the log, it is appended to the anchor's
latest `build_attempt` log so the build's log tab shows it just like a
locally-built one. If no upstream serves the log, the build is recorded
without one.

## Adaptive fetch/eval split

When the scheduler detects an idle dedicated eval-only worker - determined by
checking whether any connected worker is eval-only (fetch capability absent)
and has no currently assigned job - it splits a flake evaluation into two
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

## Waiting reasons

Every dispatch pass reconciles each in-flight evaluation against the live worker
pool and parks it in `Waiting` (with a structured `waiting_reason`) when it
cannot make progress, auto-unparking once the blocker clears:

- **Pre-build phases** - a `Fetching` eval needs a worker advertising the
  `fetch` capability; `Queued`/`EvaluatingFlake`/`EvaluatingDerivation` need an
  `eval`-capable worker. When none is connected the eval parks with an
  `eval_workers` reason naming the missing `capability`, even if it has already
  batched some builds, and recovers to `Queued` when such a worker connects
  (issue #381).
- **Build phase** - a `Building` eval parks with a `workers` reason listing the
  unmet `(architecture, required_features)` combinations when no connected
  worker can satisfy any pending build.

Approval, no-cache and full-cache parks are owned by the webhook and cache hooks
and are never unparked by the worker reconciler.

## Re-offering re-queued jobs

Job offers and scores are deltas: the server only offers a candidate a worker
has not been sent, and the worker only scores candidates new or changed against
its local cache. A build that was dispatched and then returned to the pool (a
failed/transient requeue, or a worker reject because it was draining or at
capacity) must therefore be re-offered so it is *scored a second time* -
otherwise it sits unassigned even while a worker has free capacity. Three things
make that happen: `enqueue_build_job` clears the build's sent-candidate flag on
every (re-)enqueue; the worker drops a job from its candidate + score caches on
reject (not only on accept), so a re-offer is treated as new; and the build
dispatch loop bumps the job-notify each pass while any job is pending, so a
re-queued job reaches workers (including one that just freed capacity) without
waiting for the next enqueue.
