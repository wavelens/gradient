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

A `build_job` anchor is marked complete even with zero edges, since a genuine leaf
legitimately has none - which left a hole: if `flush_deferred_deps` could not
resolve a *declared* dependency edge (the dependency derivation was never recorded,
e.g. an interrupted or overlapping eval), the edge was silently dropped yet the
source was still marked `edges_complete` and dispatched as dependency-free, failing
`InputsUnavailable` on an input the server never had. `flush_deferred_deps` now sets
`edges_unresolved` on any source whose declared edge it could not resolve, and
`mark_edges_complete_for_eval` (both the completion and graph-unstick callers)
refuses to mark those - so a 0-edge anchor that *declared* a dependency is held
until a later eval records it, while a genuine leaf still promotes. The flag is
cleared when a complete eval resolves the source's edges.

Promotion is otherwise event-driven (`promote_ready` at eval completion,
`promote_dependents` at build completion), so a ready anchor whose triggering
event never fired - a failed eval after its edges were flushed, a dependency that
completed in a missed window, a restart - would sit in `Created` forever.
`promote_ready` therefore also runs as a periodic backstop. The `edges_complete`
gate is what makes this sweep safe: it can only ever promote fully-flushed
anchors, so it can never dispatch a 0-edge anchor without its inputs.

`reconcile_dependency_failed` is the failure-side counterpart of that backstop.
The reactive `cascade_dependency_failed` fires only on a fresh terminal-failure
*transition*, so it cannot reach an anchor that becomes non-terminal **after** its
dependency already failed: `requeue_failed_anchors` / `requeue_failed_closure_for_eval`
thaw a dependent back to `Created` without re-checking its still-failed dependency,
and a concurrent evaluation can re-fail a dependency after the dependent was thawed.
Such a dependent can never build, yet the dispatch gate holds it (its dependency is
not terminal-success) and `check_evaluation_done` never finalizes its evaluation - a
permanent dead zone that strands the eval in `Building`. The sweep walks
`derivation_dependency` upward from every terminal-failed anchor and marks each
reachable non-terminal anchor `DependencyFailed` in one statement.

#### The graph reconciler and transition effects

All of the self-heal sweeps above run through one orchestrator,
`gradient_db::reconcile_build_graph(ctx, scope)`, which owns the canonical step
ordering (demotes before flag fixpoints before the failure sweep before
promotion). Its three scopes correspond to the three places healing is needed:
`Global` on the 5s dispatch tick (the periodic backstop), `Eval(id)` when an
evaluation finishes flushing its graph, and `Unstick(id)` when a Building
evaluation is graph-stuck (adds the terminal-failed thaw across its closure).
Every future dead-zone fix has exactly one place to live.

The consequences of moving an anchor are equally centralized. Bulk sweeps
return the typed `(derivation, from, to)` transitions they made, and both
mutation models - the state-machine-guarded single-row path
(`update_derivation_build_status`) and the bulk SQL sweeps - feed them through
one `emit_transition_effects`: entry-point dep-count deltas, board events, the
per-entry-point CI check, cache-changed notifications, and evaluation
finalization (`check_evaluation_done` fires for every terminal transition, from
any path). It is structurally impossible to move an anchor without its
consequences firing, which closes the historical "bulk sweep bypassed the
reactive hook" dead-zone class.

Two more definitions exist exactly once. The dependency-readiness predicate
("every dep terminal-success + `closure_complete`, or substitutable, and every
input source cached") is generated by `graph_sql::deps_ready_predicate` and
embedded verbatim by `promote_ready`, `promote_dependents`, and the dispatch
gate `find_ready_anchors`, so promotion and dispatch can never disagree on what
"ready" means. The recursive `derivation_dependency` walk is generated by
`graph_sql::dependency_closure_cte` and shared by the failure cascades, the
per-eval closure sweeps, and the GC keep-set.

A read-only consistency sweep (`graph_consistency_report`, interval
`GRADIENT_GRAPH_CONSISTENCY_INTERVAL`, default 300s) counts violations of the
invariants those gates trust - stale-true `closure_complete` /
`drv_closure_cached`, promotable-but-unpromoted anchors, unbacked
terminal-success outputs, Building evaluations with no active anchors - and
logs them as warnings, so a non-converging heal surfaces as an alert instead of
a user-reported stuck evaluation.

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

Non-substitutable anchors are finally gated on `derivation_build.drv_closure_cached`,
the `.drv`-closure analogue of `closure_complete`. A build worker cannot import a
build target's `.drv` until the `.drv`'s full reference closure (every transitive
input `.drv` plus its input sources) is in the cache - the daemon's
`add_to_store_nar` rejects a NAR with absent references. The eval pushes those
`.drv`s progressively, so without this gate a build dispatched mid-push fails
terminal `InputsUnavailable` on a missing `.drv` (its own or a dependency's) - the
dominant failure of large NixOS system-closure derivations. `reconcile_drv_closure_cached`
(run in the dispatch tick, at eval completion, and during graph-unstick) is a
bidirectional fixpoint like `reconcile_closure_complete`: a CLEAR pass first resets
any anchor whose `.drv` is no longer backed, then a SET pass marks an anchor once
its own `.drv` is cached and every build dependency is itself `drv_closure_cached`.
The CLEAR pass is load-bearing because the flag is not monotonic-safe: GC deletes a
`.drv`'s `cached_path` row once its NAR object is gone (`purge_zombie_cached_paths`),
and the post-GC `demote_unbacked_trusted_outputs` backstop only heals OUTPUT trust,
so a stale-true flag would otherwise dispatch a build whose `.drv` has vanished and
strand its whole closure in terminal `InputsUnavailable`. The recursion is
independent of build/substitute status, since a substitutable dependency's `.drv` is
still a structural reference of any dependent's `.drv`. A substitutable anchor itself
substitutes its output and never imports its `.drv`, so the gate skips it.

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

The build that reported `InputsUnavailable` retries **in-eval** rather than
failing permanently. The self-heal above resets its missing input's producer to
`Created`, so the build itself is marked `FailedTransient` (not `FailedPermanent`)
and re-queued through the normal transient backoff (`decide_failure_outcome`
treats `InputsUnavailable` like a transient failure). `dispatch_ready_builds`
re-checks every dependency at dispatch, so the re-queued build waits in `Queued`
until the rebuilt input is `Completed`/`closure_complete`, then dispatches and
succeeds - without the failure leaking onto a sibling evaluation that shares the
global anchor. The self-heal circuit breaker (`inputs_unavailable_max_loops`)
still caps the loop: once it trips, the input is deemed unrecoverable and the
build fails `Permanent` (then cascades), so a genuinely missing input can't retry
forever.

The cache GC can break the invariant from the other direction: the zombie-purge
(`cached_path` whose NAR vanished) and the TTL eviction delete `cached_path` rows
without going through `demote_cached_output`, leaving the producer at
`Completed`/`Substituted` + `closure_complete` with no fetchable output. The gate
then trusts it, dependents fail `InputsUnavailable` permanently, and - being
terminal-*success*, not terminal-failed - it is never re-queued, so it never
rebuilds. `demote_unbacked_trusted_outputs` restores the row-vs-object invariant:
it finds every terminal-success producer (`status IN (3, 7)`) with **any** output
that is neither in our cache (a `cached_path` with a NAR) nor on an upstream
(`external_url`) and demotes it back to `Created`. It keys on the **ground truth**
(a missing backing NAR), **not** the derived `is_cached` flag nor
`closure_complete`. Both derived flags are `false` for exactly the dead-zone
anchors this sweep must rescue: `closure_complete` is cleared by the bidirectional
`reconcile_closure_complete` once an object vanishes, and `is_cached` is `false`
whenever an anchor was marked `Completed` with an output that was *never* cached -
a partial cache-hit or substitution that set the anchor done without backing every
output (observed on multi-output CUDA derivations whose `out` was never pushed, no
build attempt). An `is_cached`-gated predicate skipped that case, stranding the
producer and its whole dependent subtree. The completion path records each output's
`cached_path` before flipping the anchor terminal (#303/#399), so a
genuinely-complete anchor is never demoted mid-completion. It runs hourly in the
cache loop (after the GC passes) and inside the reconciler's `Global` and `Eval`
scopes, so an orphaned or partially-cached producer heals promptly - even while
the evaluation that needs it is itself stuck `Building` - without manual
intervention.

GC deletion also maintains the dispatch-gate invariant inline instead of leaving
it to the next reconcile tick: every pass that deletes `cached_path` rows
(orphan-derivation GC, zombie purge, TTL eviction, path invalidation) clears the
`drv_closure_cached` / `closure_complete` flags those rows backed **in the same
transaction** (`clear_gate_flags_for_hashes`), so there is no window in which the
gate trusts an artifact GC just removed. Path invalidation goes further and
demotes the producer itself (`demote_cached_output`), so an invalidated output
rebuilds instead of staying trusted-but-gone. And because the per-project
evaluation GC refuses to run while any evaluation is active, a wedged `Building`
evaluation used to freeze a project's GC forever - an "active" evaluation
untouched for `gc_wedged_eval_hours` (default 24h) now stops blocking, while
never being deleted itself.

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

The keep-set is built from committed DB rows, so it cannot reference a NAR that is
already on disk but whose `derivation`/`cached_path` rows have not been written yet
- the in-eval window between the worker's presigned `.drv` PUT and the server
processing its `NarUploaded`. The orphan-files pass therefore **spares any NAR
younger than the upload grace** (`nar_upload_grace_hours`, its own knob so it no
longer shares a meaning with the derivation-row grace): reclaiming a just-pushed
`.drv` in that window
left a *zombie* `cached_path` (row committed moments later, object already gone)
that the dispatch gate trusts as the cached `.drv`, so `push_drv_closure` skipped
re-pushing it (CacheQuery reported it cached) and dependent builds failed
`InputsUnavailable` on a `.drv` that was never re-uploaded. The grace closes the
upload-vs-GC race; a NAR that is still unreferenced after the window is a genuine
orphan and reclaimed.

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

An anchor flagged `edges_unresolved` is never pruned either, even with all outputs
on an upstream: its edge set is known-incomplete (a dependency a prior eval could
not record - e.g. GC'd from a shared closure), and pruning it would skip the walk
that rediscovers the dropped edge and clears the flag, stranding it and its
dependents off promotion forever. Forcing the re-walk is what makes the flag's
"a later eval resolves it" contract actually hold.

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

`propagate_closure_complete` only ever *sets* the flag, which is unsound on its
own: the flag would then survive a closure member being demoted/evicted, or a
dependency edge recorded after the anchor was already marked complete (a dependent
instantiated before its dependency). A stale-true flag dispatches a build whose
transitive closure is not actually cached - terminal `InputsUnavailable` on a tiny
transitive output (e.g. `unit-*.service` reached through a direct dep). The
**bidirectional** `reconcile_closure_complete` keeps the flag honest: a CLEAR
fixpoint resets any anchor whose gate no longer holds (output uncached, a
dependency regressed, or a newly recorded dependency not itself complete) before a
SET fixpoint re-marks the genuinely satisfied. Both ripple over
`derivation_dependency` and converge in O(longest affected chain); it runs at eval
completion, graph-unstick, and the 5s dispatch tick so the gate below never reads a
stale flag. The reactive `clear_closure_complete_for_referrers` (below) still fires
on demote, but the periodic reconcile is the backstop that does not depend on the
demote walk finding a not-yet-recorded edge.

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

A **corrupt cached NAR** feeds the same self-heal. The worker verifies every
fetched input NAR against its recorded `nar_hash`/`nar_size` before importing it.
A mismatch means the bytes in our object store do not match the metadata we sign
and serve - the object and its `cached_path` row were written by different
producers and desynced. This happens with non-reproducible builds: a path built
locally (its NAR differs from upstream, e.g. an embedded `.git/index` ctime) can
end up hosted under a `cached_path` whose hashes were recorded from an
upstream-substitute relay, because object writes (presigned PUT) and metadata
writes (`NarUploaded`) are independent. The worker reports the failing path as a
`CorruptCachedNar`, which the executor classifies as `InputsUnavailable` (not a
transient retry against poison), so `reconcile_missing_inputs` purges the bad
object and rebuilds the producer with consistent metadata. Verify-on-read makes
the cache self-correcting regardless of how a desync arose.

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
