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

Only a *genuine* miss counts toward the substitute-miss budget. The worker reports
`SubstituteUnavailable` (escalation-eligible) only when an output is on no upstream;
a transient relay failure - the Pull RPC timing out, the NAR download, or the
presigned PUT into our own store - is reported as a retryable `Transient` instead.
So a couple of unlucky infra timeouts can no longer escalate a substitutable build
into a from-scratch one (whose `.drv` may never have been pushed). The probe pool
also bounds how long a single narinfo probe waits for a permit, so a large eval
flooding the shared query semaphore can't make a build's cache lookup block past
its 120s deadline.

A terminal-*success* anchor (`Completed`/`Substituted`) encodes the invariant
"this output's NAR is fetchable". When that artifact is removed -
`demote_cached_output` (purging a stale/zombie `cached_path`, or self-healing a
NAR missing from storage, or `reconcile_missing_inputs` after a build reported its
inputs unfetchable) deletes the NAR - the invariant no longer holds, so demote
also resets the producing anchor back to `Created` (a real build, not a
re-substitute of the deleted artifact). Without this the producer would stay
"succeeded" forever and every dependent fail `InputsUnavailable` indefinitely; the
reset lets it rebuild and the next eval re-marks it substitutable if it is
genuinely still on an upstream. The reset clears the output's whole availability
record - `is_cached` **and** `external_url` - not just the our-cache half: leaving
`external_url` set keeps the node prune-eligible (pruning keys on `external_url`,
not `is_cached`), so the next eval skips re-walking it, never re-pushes its `.drv`,
and the reset-to-build anchor dead-ends on a missing `.drv`.

When the demanded output's producer is instead terminal-*failed*
(`FailedPermanent`/`Aborted`/`FailedTimeout`), `reconcile_missing_inputs`
re-queues it on the spot (`requeue_failed_anchors` over the demoted producers):
the dependent that just failed is a fresh build intent, so the producer retries
immediately rather than waiting for a new evaluation - which matters when evals
are being aborted and would otherwise never requeue it, leaving the dependent
dead-ended on `InputsUnavailable`.

The cache GC can break the invariant from the other direction: the zombie-purge
(`cached_path` whose NAR vanished) and the TTL eviction delete `cached_path` rows
without going through `demote_cached_output`, leaving the producer at
`Completed`/`Substituted` + `closure_complete` with no fetchable output. The gate
then trusts it, dependents fail `InputsUnavailable` permanently, and - being
terminal-*success*, not terminal-failed - it is never re-queued, so it never
rebuilds. `demote_unbacked_trusted_outputs` restores the invariant: it finds every
gate-trusted producer (`status IN (3, 7) AND closure_complete`) whose output is
neither in our cache (a `cached_path` with a NAR) nor on an upstream
(`external_url`) and demotes it back to `Created`. It runs hourly in the cache loop
(after the GC passes) and once at eval-resolve before promotion, so a producer the
GC orphaned heals on the next evaluation without manual intervention.

The deeper cause of those orphans is the derivation GC itself. `build_job` rows are
per-evaluation and pruned with old evals (`keep_evaluations`), but the global
`derivation_dependency` graph and the build-once anchors persist. The old orphan
pass treated "no `build_job`" as "unreferenced" and deleted the derivation (its
`.drv` + output NARs + `cached_path` rows), so a derivation still needed as a build
input of a retained closure - its own evals long gone - got swept away, stranding
dependents on `InputsUnavailable`. `gc_orphan_derivations` is now a mark-and-sweep:
it reclaims a derivation only when it lies *outside the build-dependency closure of
every live root* (`entry_point` ∪ `build_job` derivations, walked over
`derivation_dependency`). The orphan-NAR keep-set (`active_hashes`) likewise pins
the input sources and `.drv` hashes of every derivation with a build anchor - not
just outputs, and *regardless of build status*. These are producerless (only an eval
re-pushes them), so a terminal-failed anchor a later eval requeues must still find
its `.drv`; gating that clause on status purged the `.drv` of a failed-but-requeueable
build and dead-ended its retry on `InputsUnavailable`. Outputs stay status-gated (they
are rebuildable, TTL-evicted), and `gc_orphan_derivations` reclaims a derivation's
`.drv`/sources once it leaves the live closure.

Evaluation GC (`gc_project_evaluations`) deletes old evaluations and relies on FK
cascade to clear their per-eval rows: `evaluation -> build_job -> build_attempt`.
`build_log_chunk` previously carried a bare `build_attempt` UUID with no FK, so its
chunk-index rows leaked forever once the eval (and its attempts) were collected; it
now cascades from `build_attempt`, completing the chain. The log blob itself is still
removed explicitly (it is object storage, not FK-tracked). `dispatched_job` and the
metrics firehose (`phase_event`, `worker_sample`, `metric_rollup`) are pruned by age
in the separate retention loop instead.

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
relays it verbatim when it is already zstd-compressed at our 2 MiB level-6
window (else recompresses), and pushes it into the gradient cache (`use_substitutes` stays
off in the daemon - substitution always goes through gradient, never the worker's
own nix config). Existing build-once anchors a prior eval left not-yet-succeeded
are flipped substitutable when an upstream is newly found, so a previously-failed
fetcher substitutes instead of rebuilding.

Upstreams are probed in hit-rate-then-latency order (most-likely cache first; never-probed
upstreams are tried last) so the first hit wins cheaply. The lowest-latency holder's URL is
persisted on `derivation_output.external_url`. Total outbound probe concurrency is bounded
server-wide by `GRADIENT_UPSTREAM_QUERY_CONCURRENCY` (default 32).

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
(`QueryKnownDerivations`); the server prunes a subtree only when **every** output
is on a real upstream cache (`external_url`). An upstream binary cache serves a
*complete closure*, so a build worker can fetch the pruned subtree's outputs on
demand. Our own cache (`is_cached` / `cached_path`) is deliberately not accepted
for pruning: it is populated output-only (substitution relays just the output NAR,
and a config-specific node's subtree may never have been pushed), so pruning on it
would strand that subtree - never walked, recorded, or built, and off-upstream so
unfetchable, a permanent `InputsUnavailable` dead-end (e.g. `unit-*.service` ->
`X-Restart-Triggers-*`, which exist on no upstream). nixpkgs still prunes via its
persisted `external_url`; the worker re-walking our own (unreliable) cached
closures is the correctness price of an output-only cache.

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

`derivation_build.closure_complete` means a **built** anchor's whole build
closure is fetchable: its outputs are cached, its edges are flushed
(`edges_complete`), and every build dependency is itself `closure_complete` **or**
`substitutable` (its closure lives on an upstream cache, fetchable on demand). A
build's runtime references are a subset of its build inputs, so a fetchable build
closure guarantees a fetchable runtime closure too - closing the runtime-vs-build
edge gap without a runtime walk.

`propagate_closure_complete` (called from `update_derivation_build_status` the
moment an anchor reaches terminal success, before it promotes dependents) computes
this over `derivation_dependency` and **ripples up**: it marks the just-finished
anchor complete if its deps are all satisfied, then re-checks that anchor's
dependents, and so on. The up-ripple is essential - a dependent that finished
before its dependency did would otherwise never re-evaluate its own completeness.
A **substituted** anchor is deliberately *not* marked complete (we hold only its
output NAR, not its build closure); dependents reach it through the `substitutable`
arm instead.

`promote_ready`, `promote_dependents`, and `dispatch_ready_builds` therefore gate
each dependency on `(status IN (Completed, Substituted) AND closure_complete)`
**or** `substitutable`. A `substitutable` anchor skips the dependency gate
entirely and dispatches out of order (#456); its substitute job carries no
`required_paths`, so the worker pulls no build deps and the job scores a uniform
zero. The gate stays O(1) (a flag check), and the propagation touches only the
completing anchor's dependent sub-tree. Partial indexes on `derivation_build`
keyed by the dispatch (`status = Queued AND edges_complete`) and promote
(`status = Created AND edges_complete`) predicates keep the per-tick scans off the
full anchor table.

When a build still reports a path missing, `reconcile_missing_inputs` self-heals:
a missing leaf with a producer is purged + rebuilt (`demote_cached_output`) and
`closure_complete` is cleared up the referrer chain
(`clear_closure_complete_for_referrers`) so dependents re-block until the leaf
re-pushes closure-complete; a producerless source (no producer to rebuild)
demotes its direct **referrers** (`demote_referrers_of`) so a referrer rebuild
re-pushes it. The migration backfills the flag to a fixpoint over the existing
cache and resets any closure-incomplete terminal anchor so it rebuilds.

An **orphan producer** is the third case: the missing leaf has a producing
derivation, but that producer has no `build_job` (it was pruned out of the build
graph because a referrer's output was cached without its closure under output-only
substitution), so promotion can never queue it and the gentle flag clear leaves
the referrer cached, pruned, and never re-walked. When `demote_cached_output`'s
producer is not reachable (`derivation_is_reachable` is false), the referrers are
demoted (`demote_referrers_of`) so the next eval re-walks them, re-records the
dropped edge, and schedules the orphan. Demote leaves `edges_complete` intact: it
deletes the `cached_path`, so the output is uncached and the next eval re-walks the
derivation regardless (uncached nodes are never pruned) - clearing the flag would
only strand a complete-edge node behind the closure gate until that re-walk.

An **absent orphan** is the fourth case and the one that makes the whole thing
self-heal without operator surgery: the missing input has *no* producer row and
*no* indexed referrer (it was pruned out so thoroughly it was never recorded, or
an admin deleted its rows), so it cannot be reached upward at all. Instead it is
reached downward from the known failing build: `demote_output_only_cached_deps`
demotes that build's output-only-cached direct dependencies (output present in our
cache, no `external_url`), forcing the next eval to re-walk them and re-record the
orphan plus its now-buildable subtree. Upstream-fetchable deps (`external_url`) are
left untouched, since a real upstream serves their closure whole. So an accidental
cache-row deletion recovers on the next evaluation rather than requiring a manual
reset.

A **circuit breaker** bounds this self-heal. Each `InputsUnavailable` failure
reconciles the cache so the next eval rebuilds the input; a genuinely unrecoverable
input turns that into a hot loop that re-purges and re-pushes the same closure
forever. `inputs_unavailable_attempt_count` counts the anchor's prior
`InputsUnavailable` attempts (a distinct `build_attempt.reason`), and once it
reaches `GRADIENT_INPUTS_UNAVAILABLE_MAX_LOOPS` (default 3) the build fails fast
without reconciling - the eval reports a clear permanent failure instead of
churning the cache. Every failure also persists the worker's error on
`build_attempt.failure_message` (capped, full text still in the log) so the cause
is visible without opening the log.

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
- **Graph stuck** - the pool *can* build every pending anchor (so the `workers`
  reason would carry an empty `unmet` set) yet none is dispatchable: the whole
  pending set is `Created`, blocked behind the `closure_complete` gate with no
  in-flight build to drive a promotion. `propagate_closure_complete` only fires
  on completion events, so it can never reach this deadlock. The reconciler
  detects it and self-heals in four steps: `requeue_failed_closure_for_eval`
  thaws any terminal-failed anchor in the eval's full dependency closure (a
  transitive dep a prior eval left failed and this eval pruned has no `build_job`
  here, so `requeue_failed_anchors` never reaches it and it blocks its dependents
  with no dispatch to fail); `reconcile_cached_anchors_for_eval` marks every anchor
  in the closure whose outputs are all in our cache `Completed` + `closure_complete`
  (build-graph state desyncs from the durable cache state - a derivation whose
  artifacts exist sits `Created` after a requeue/cascade/demote and blocks its
  dependents, so cache presence is trusted as the ground truth); then the
  `reconcile_closure_complete` fixpoint and a re-promote. It re-assesses: recovers
  to `Building` when the heal frees an anchor, else parks `graph_stuck` (the blocked
  count) and retries each pass.

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
