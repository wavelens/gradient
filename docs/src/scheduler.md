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

Promotion is otherwise event-driven (`promote_ready` at eval completion,
`promote_dependents` at build completion), so a ready anchor whose triggering
event never fired - a failed eval after its edges were flushed, a dependency that
completed in a missed window, a restart - would sit in `Created` forever. The
build dispatch loop therefore runs `promote_ready` once per timer tick as a
backstop. The `edges_complete` gate is what makes this periodic sweep safe: it
can only ever promote fully-flushed anchors, so it can never dispatch a 0-edge
anchor without its inputs.

Promotion and dispatch are finally gated on a derivation's `inputSrcs` being in
the cache. A `.drv`'s build-time source paths (`inputSrcs`, e.g.
`builtins.toFile` configs) have no producing derivation, so the dependency-anchor
check does not cover them; they are recorded per derivation in
`derivation_input_source` (parsed from the `.drv` at `report_eval_result`) and a
non-substitutable anchor is promotable only when every one of its sources is
`fully_cached`. Without this a requeued anchor - reset to `Created` but still
`edges_complete` with all dependency anchors cached - would re-dispatch the
instant the periodic backstop runs, before the new evaluation re-pushed its
sources, and fail `InputsUnavailable`; with the gate it waits for the walk to
push them. A substitutable anchor needs no sources, since it fetches its outputs
directly, so the gate skips it.

Because the anchor is global and build-once, a new evaluation is treated as a
fresh build intent: `resolve_anchors` re-queues anchors a previous eval left
terminal-failed, and the substitute-miss budget is scoped per evaluation. A
permanent failure (or an exhausted substitute budget) therefore does not poison
every later evaluation that needs the derivation - the world (upstream cache,
network) may have changed since it failed.

A terminal-*success* anchor (`Completed`/`Substituted`) encodes the invariant
"this output's NAR is fetchable". When that artifact is removed -
`demote_cached_output` (purging a stale/zombie `cached_path`, or self-healing a
NAR missing from storage, or `reconcile_missing_inputs` after a build reported its
inputs unfetchable) deletes the NAR - the invariant no longer holds, so demote
also resets the producing anchor back to `Created` (a real build, not a
re-substitute of the deleted artifact). Without this the producer would stay
"succeeded" forever and every dependent fail `InputsUnavailable` indefinitely; the
reset lets it rebuild and the next eval re-marks it substitutable if it is
genuinely still on an upstream.

When the demanded output's producer is instead terminal-*failed*
(`FailedPermanent`/`Aborted`/`FailedTimeout`), `reconcile_missing_inputs`
re-queues it on the spot (`requeue_failed_anchors` over the demoted producers):
the dependent that just failed is a fresh build intent, so the producer retries
immediately rather than waiting for a new evaluation - which matters when evals
are being aborted and would otherwise never requeue it, leaving the dependent
dead-ended on `InputsUnavailable`.

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

As the worker walks the graph it pushes each produced `.drv`'s runtime closure
to the cache before reporting its batch, so a build dispatched mid-evaluation
finds its inputs already present. A `.drv`'s build-time input sources
(`inputSrcs` - e.g. `builtins.toFile` configs) have no producing derivation and
cannot self-heal if missing, so they are discovered by parsing each `.drv`
directly rather than via the daemon's reference walk, which does not reliably
report them; this mirrors the build-side prefetch so every source a build worker
will demand is guaranteed pushed by the evaluation that produced it.

The eval closure walk prunes the same way. As the worker walks the graph it
asks the server which dependency derivations it already knows
(`QueryKnownDerivations`); the server prunes a subtree only when the derivation's
complete output set is fetchable without building it - every output is in our
cache (`is_cached`, or its hash recorded in `cached_path`) or on a known upstream
(`external_url`). This mirrors the eval-time substitutability decision, so the
worker skips re-walking the whole upstream closure (e.g. nixpkgs) on every
evaluation instead of descending into subtrees it will never build.

#### Closure-complete cache

The cache holds a binary-cache invariant: *if an output is in our cache, its
entire runtime closure is too*. A build (and a substitution, which fetches the
output's closure locally first) pushes the **full runtime closure** of its
outputs, not just the output paths; already-cached members are skipped, so only
paths the cache is actually missing upload.

The dispatch gate does not merely *trust* this - it **enforces** it. A build's
build-time dependency edges (`derivation_dependency`) do not include a dep's
transitive runtime references, so "dep is Completed/Substituted" alone does not
guarantee the dep's runtime closure is fetchable. A dependent dispatched on that
weaker signal fails `InputsUnavailable` on a runtime path the gate never checked
(e.g. `nixos-system` needs `unit-bird.service` via `system-units`, which has no
direct edge). So completeness is tracked explicitly:

- `cached_path.closure_complete` - a NAR is complete once present **and** every
  non-self reference is itself present and complete.
- `derivation_build.closure_complete` - true once all of a terminal-success
  anchor's outputs are complete.

Both are set in one deterministic pass by `mark_closure_complete`, called from
`update_derivation_build_status` **the moment an anchor reaches terminal success,
before it promotes dependents**: `compress_and_push_paths` has by then pushed the
anchor's **full runtime closure**, so the pass BFS-walks that closure over
`cached_path.references`, takes the fixpoint of "present and every ref present +
complete" within it, bulk-sets the flag, and marks every anchor whose outputs are
all complete (Nix produces all of a derivation's outputs together). Finalizing
before promotion is essential: the last dependency to land must carry the flag at
the instant its dependents are gated, or they stall behind a flag that flips only
afterward.

`promote_ready` and `dispatch_ready_builds` require every dependency to be
`status IN (Completed, Substituted) AND closure_complete`, and
`compute_truly_substituted` only marks an output Substituted when its cache entry
is closure-complete. The gate stays O(1) (no hot-path closure walk) because the
flag amortizes the check. Partial indexes on `derivation_build` keyed by the
dispatch (`status = Queued AND edges_complete`) and promote
(`status = Created AND edges_complete`) predicates keep these per-tick scans off
the full anchor table; `mark_closure_complete` prunes its BFS at already-complete
subtrees so each build finalizes in O(new paths), not O(closure).

When a build still reports a path missing, `reconcile_missing_inputs` self-heals:
a missing leaf with a producer is purged + rebuilt (`demote_cached_output`) and
`closure_complete` is cleared up the referrer chain
(`clear_closure_complete_for_referrers`) so dependents re-block until the leaf
re-pushes closure-complete; a producerless source (no producer to rebuild)
demotes its direct **referrers** (`demote_referrers_of`) so a referrer rebuild
re-pushes it. The migration backfills the flag to a fixpoint over the existing
cache and resets any closure-incomplete terminal anchor so it rebuilds.

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

## Startup recovery

A server restart kills every in-flight job, so `recover_interrupted_work` runs
once at startup to reconcile the durable state the dead process left behind:

- Orphaned `Running` build attempts are marked `Aborted`.
- `Building` anchors are re-queued to `Queued` for re-dispatch (their evaluation
  reached the build phase, so their edges are already flushed).
- Pre-build in-flight evaluations (`Fetching`/`EvaluatingFlake`/
  `EvaluatingDerivation`) are aborted - their dependency edges were never
  flushed - and their projects get `ForceEvaluation` so a fresh evaluation
  re-walks them and writes a complete graph.
- The anchors those aborted evaluations drove are aborted too
  (`Created`/`Queued`/`Building` -> `Aborted`), mirroring the explicit-abort
  path: the builder aborts the evaluation's builds when the server dies, so the
  server reflects it. A global build-once anchor a still-live evaluation also
  needs is left running (shared-anchor safety). The forced re-evaluation
  re-drives the aborted anchors - `requeue_failed_anchors` resets them to
  `Created` - and they promote once their edges are flushed.
