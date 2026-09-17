# Scheduler

The Gradient scheduler coordinates build dispatch across connected workers.
This page covers how builds are shared across evaluations and projects.
For a general overview of the scheduler architecture see
[Architecture](development/architecture.md).

### Shared build anchors

A derivation is built exactly once, globally. Build state lives on a
`derivation_build` anchor keyed 1:1 to the content-addressed `derivation`
(UNIQUE on `derivation`, so the database itself enforces build-once). Every
evaluation that needs a derivation gets a per-eval `build_job` linking it to
that anchor; each execution attempt and its log live on `build_attempt` under
the anchor. `build_attempt.build_job` is `ON DELETE SET NULL`, so an attempt
outlives the evaluation that drove it - its true owner is the build-once anchor,
and a `Completed` anchor reused by a later evaluation keeps a retrievable log
until the derivation itself is GC'd.

When two evaluations - in the same or different projects - need the same
derivation, they share the one anchor: whichever is dispatched first builds
it, and the others observe the result the moment the anchor reaches a
terminal-success status. There is no leader/follower row and no `via` link;
sharing is implicit in the global derivation graph.

#### The graph actor

Every write to the anchors, edges, outputs, input sources, build jobs,
attempts and the cache index (`cached_path`, `cached_path_reference`) is a
message to one actor, `graph`, under the root of the supervision tree. One
message is one transaction:

| message | what it writes |
|---|---|
| `Ingest` | one worker batch: derivations, outputs, input sources, upstream hits, anchors, build jobs, features, messages, entry points, and the edges that resolve so far |
| `KnownDerivations` | nothing; a read answered after every batch queued before it |
| `CommitNar` | the `cached_path` row, its references, signature placeholders and the outputs it backs |
| `Transition` | an anchor or evaluation state change: build started, output, completed, failed, dispatched, orphaned, ready, a reconcile scope, an abort |
| `Requeue` | transient retries whose backoff elapsed |
| `Demote` | a missing or invalidated NAR, a cache dropping its claim, the unbacked-output sweep |

Consecutive batches from different evaluations that are queued at the same
time are written in one transaction with a savepoint per batch, up to 5000
derivations, so one bad batch fails only its caller. A caller blocks until its
reply (ten minutes), which is the backpressure that stops a worker's reader
instead of dropping its batch; the actor's own work is bounded at 120 s per
transaction, past which it rolls back and the caller sees an error.

The facts a batch needs from outside the graph are established by the
scheduler before the message is sent: which derivations are already whole in
the cache and which an upstream serves (the narinfo probe). They are keyed by
drv path and output hash; ids are assigned inside the transaction.

Merging graphs from concurrent workers. Two evaluations of overlapping graphs
converge on one `derivation` row per hash because the existence check and the
insert are one serialized transaction; edges are content-addressed and
conflict-ignored, so two walks of a node record the same set; an evaluation
whose batch fails is marked `Failed` rather than left with a hole, because the
wire carries no acknowledgement a worker could retry on; and a batch for an
evaluation that is not streaming (terminal, `Building`, parked) is dropped, so a
worker that died mid-walk cannot merge late batches into the re-dispatched walk
once it has completed. Two walks of the *same* evaluation are told apart by the
dispatch id the scheduler mints at assignment: the worker echoes it on every
report, and a report naming a dispatch its session did not hand out is dropped.
The one fact a batch writes globally is `substitutable`, only ever set, never
cleared, and only on an anchor that has not yet succeeded, so one evaluation's
upstream probe can add a substitution for another walk but never take one away.

Nothing is deferred between messages, so a restart leaves no pending graph
state: a batch's stubs, records, edges, anchors and jobs are one transaction
that either lands whole or fails to its caller, and a batch still queued for the
next flush fails the same way.

#### Promotion

Readiness is two maintained columns on the anchor. `fetchable` says a dependent
can get this anchor's outputs **from our own cache**: the anchor succeeded and
every output is whole here (`cached_path.missing_references = 0`). An upstream
copy does not count - a dependent of an unrelayed substitutable anchor waits for
the relay - so a build pulls every input out of our cache and the relay is on the
critical path once instead of every dependent re-fetching upstream itself.
`unready_deps` is how many of the anchor's direct dependencies are not fetchable.
Neither is ever derived by a sweep: the event that changes fetchability writes
the flip with a `RETURNING` that names exactly the anchors that changed, and
their direct dependents' counter moves from that set in one statement
(`gradient_db::readiness`). Fetchability of a dependent does not depend on its
own dependencies, so nothing recurses over `derivation_dependency`; the only
recursion left is the reference ripple on the NAR side.

An anchor is promoted `Created` to `Queued` when its derivation is walked, some
evaluation wants it (a `build_job`), and something still demands it, and then one
arm per kind of work: a build needs `unready_deps = 0` and its own `.drv` whole in
the cache, a relay needs nothing more. Those terms are
`graph_sql::gates_predicate`, generated once. The events that can open one promote
the anchors they touched: a batch that walked it, a dependency becoming fetchable,
its `.drv` NAR arriving, an upstream hit, a thaw at stream completion.

Demand is what keeps the fleet from relaying half of nixpkgs, and from building the
input closure of everything it relays. An anchor is demanded when an entry point of
a retained evaluation names it, or a dependent that will itself be built (walked,
not substitutable, pending, with a `build_job`) lists it as an input and is itself
demanded. That last clause is a fixpoint, so demand is a column,
`derivation_build.demanded`, and not a subquery: it flows DOWN from the entry
points while readiness flows UP from the leaves, and a per-row predicate that looks
one hop cannot carry the downward direction. It used to look one hop, which is why
a relayed anchor's whole source closure was still built.

`readiness::recompute_demand` rewrites the column absolutely over the anchors an
event changed and the pending closure below them. The walk steps out of named
builders only: a relay is reached and never stepped through, because it fetches
finished bytes and needs nothing below it. The region includes the anchors it was
given, because a thaw makes one a builder again and its own stored value is as
stale as its subtree's. One statement serves both directions and returns each row
with its new value, so the caller queues what gained demand and releases what lost
it. An anchor already `Building` is left to finish: the bytes it produces are
cached and useful, while an abort throws the work away.

What nothing demands is never promoted and never built, so it is settled work and
not pending work. Both readers of "is this evaluation still waiting" -
`check_evaluation_done` and the scheduler's pending set - ask
`graph_sql::blocks_evaluation`, which counts a pending anchor only while something
demands it, and always counts one that is `Queued` or `Building`, because the
dispatcher hands out work on the status alone. An evaluation that counted the rest
would wait forever, because the event it waits for is the one that is never
coming. A demand loss settles work without moving any status, so the emitter asks
the evaluations that name what lost it whether they are done; no anchor of theirs
need have transitioned at all.

Every transition that carries an anchor into or out of the builder statuses
(`Created`, `Queued`, `Building`, `FailedTransient`) recomputes from it, and the
transition-effects emitter is where that happens - the same one place the graph
version bump and the board events fan out from, so a new mover cannot forget it.
The events that change what an anchor IS at an unchanged status call it
themselves: ingest (a newly walked builder, a new entry point, an anchor an
upstream just claimed), `demote_cached_output`, an exhausted substitution, the
per-task evaluation GC (an anchor whose last `build_job` went away) and the
reconciler's adoption, because naming is half of what demand means.
`readiness::recount_demanded` in the consistency sweep recomputes every pending
anchor from the entry points and reports what disagreed as `demand_drift`: the
backstop for a lost recompute, and the backfill the migration deliberately does not
carry.

The dispatcher does not re-derive the gates - it reads the status - so `Queued`
carries the claim that they held. That rests on one rule, which every writer of
`Queued` obeys in one of two ways: embed `graph_sql::promotable_predicate` in
the write, or settle the rows it just wrote with `readiness::unpromote_ungated`
in the same call. `readiness::repair_pending` is the backstop for a counter that
drifted under a lost move, and the only one.

When a gate regresses (an output retired, a producer demoted, a dependency
deleted by the GC) the same ripple runs in reverse, and the statement that
raises a dependent's `unready_deps` demotes it to `Created` inline, so no queued
anchor outlives the counter that gated it. The dispatcher re-reads the anchor's
status once more at hand-out, so a job the tracker still holds from before the
regression is dropped instead of dispatched.

Reachability is one of the four gates: an anchor is queued and dispatched only
while some `build_job` references its derivation. A batch names what it walked
and the direct inputs of that, and a walk prunes on `walked` alone, so the
interior of a subtree another evaluation walked first is named by that
evaluation and by nobody else; the pruning evaluation waits on it through the
pruned root, whose own gate reads the interior's readiness. Without the gate,
promotion would queue derivations no surviving evaluation needs, leaving the
dispatcher unable to attribute the build to a driving evaluation.

That sparse naming holds only while the walking evaluation lives. When
`keep_evaluations` deletes it while the pruning one is still `Building`, the
interior's names cascade away with it and every reader of the row - the gate,
the dispatch select, the dispatcher's driving evaluation, eval-done, the
abort's shared set - loses the subtree at once (#663). The GC therefore hands
the names over before it settles the queue: `reachability::adopt_pending_closures`
walks from every `build_job` of a live evaluation on a builder (walked, not
substitutable, in a builder status) down through builders into every anchor
still in a builder status, and inserts the `(evaluation, derivation)` rows that
are missing. A relay is reached and never walked through, since nothing below
it is waited on, and a terminal anchor stops the walk the same way. The walk
is bounded by pending work, never by closure size, and runs only when a name
the deletion cascaded belonged to a pending anchor. The graph reconciler runs
the same walk for the one evaluation it heals, because a thaw or a reset inside
a pruned closure leaves a pending anchor unnamed the same way, and the
consistency sweep is the backstop, walking only when a pending anchor nobody
names sits one edge below a builder a live evaluation names.

`derivation.walked` is what makes the counters safe on a graph that is still
being written. A batch names its dependencies by path, and the graph actor
inserts a stub row for every name it does not have yet, so every declared edge
lands in the same transaction as the derivation that declares it. A derivation
is `walked` once its own record is in: outputs, every edge, input sources. A
stub is never promoted, dispatched or pruned: its subtree is not recorded, and
treating it as dependency-free would dispatch a build without its inputs. The
bit is content-addressed - edges never change once written - so a later requeue
keeps the derivation promotable without re-evaluation, and a counter seeded over
a partial edge set can never be read as zero. Exactly one event clears it
again.

The one event that can invalidate the bit is the derivation GC deleting a
derivation another one still depends on: the FK cascade drops the edge and
leaves the surviving dependent `walked` over an incomplete edge set. The GC
therefore snapshots the edges into its candidates before the delete and clears
`walked` on exactly the dependents that survived a reclaimed dependency, then
re-checks the gates and pulls any of them that was `Queued` back to `Created`,
so the next evaluation re-walks them.

A failed dependency cascades `DependencyFailed` over the global
`derivation_dependency` graph reactively. That cascade fires only on a fresh
terminal-failure *transition*, so it cannot reach an anchor that becomes
non-terminal **after** its dependency already failed: `requeue_failed_anchors` /
`requeue_failed_closure_for_eval` thaw a dependent back to `Created` without
re-checking its still-failed dependency, and a concurrent evaluation can re-fail
a dependency after the dependent was thawed. The closure-bounded
`reconcile_dependency_failed`, at stream completion and on the graph-stuck heal,
catches exactly those: it walks `derivation_dependency` upward from every
terminal-failed anchor in the evaluation's closure and marks each reachable
non-terminal anchor `DependencyFailed` in one statement.

Neither walk enters a substitutable anchor. A relay takes finished bytes off an
upstream, so an input that can never build neither dooms it nor reaches anything
above it, and the requeue's blocked set stops at the same boundary so the thaw and
the cascade cannot disagree about who a failure reaches.

#### The graph reconciler and transition effects

Two heals exist for state no event can reach, and both run inside the graph
actor (`Transition::Reconcile`), so a reconcile never interleaves with an
ingest. There is no tick-driven scope: the readiness counters are moved by the
event that changes them, so the dispatch tick dispatches and reconciles nothing.

Both run through one orchestrator,
`gradient_db::reconcile_build_graph(ctx, scope)`, which owns the canonical step
ordering, and both scopes name an evaluation, so every statement it issues is
bounded to that evaluation's dependency closure: `Eval(id)` when an evaluation
finishes flushing its graph, and `Unstick(id)` when a Building evaluation is
graph-stuck. Each thaws the terminal-failed anchors in the closure, settles the
anchors whose outputs are already whole (cache presence is the ground truth for
"built") and advances their dependents' counters, fails the dependents of a
deterministic failure, names for the evaluation every pending anchor it
reaches through builders (`reachability::adopt_pending_closure`), and
promotes the closure; `Unstick` also demotes a
trusted producer whose output is gone. No step here iterates to convergence.
Every future dead-zone fix has exactly one place to live.

The consequences of moving an anchor are equally centralized. Bulk sweeps
return the typed `(derivation, from, to)` transitions they made, and both
mutation models - the state-machine-guarded single-row path
(`update_derivation_build_status`) and the bulk SQL sweeps - feed them through
one `emit_transition_effects`: the evaluation graph version, board events, the
per-entry-point CI check, cache-changed notifications, and evaluation
finalization (`check_evaluation_done` fires for every terminal transition, from
any path). It is structurally impossible to move an anchor without its
consequences firing, which closes the historical "bulk sweep bypassed the
reactive hook" dead-zone class.

`evaluation.graph_version` is the invalidation key of the task page's
per-entry-point histogram. The emitter bumps it once per emit for every
evaluation a moved anchor belongs to; an ingest batch bumps its own evaluation
and, because edges are global, every evaluation that already holds one of the
derivations whose edge set grew; startup recovery bumps what it requeues and what
it aborts, because it has no emitter. The histogram itself is computed on demand
by the root-attributed fenced walk (`task_board::entry_point_dep_counts`) for the
entry points of one page, stored in `entry_point_dep_count`, and each entry point
records the version and the time it was computed at
(`entry_point.dep_counts_version`, `dep_counts_computed_at`). No table grows with
roots times closure and no per-transition write is proportional to the
evaluation.

The version alone would be a cache that never hits: a building evaluation moves
anchors continuously, so every poll of the page would find every entry point
stale and re-walk page-size-times-closure. A read therefore recomputes an entry
point only if the graph moved AND its rows are older than
`DEP_COUNTS_REFRESH_SECS`, so the walk runs on a bounded cadence however many
viewers a page has, and unconditionally once its rows pass
`DEP_COUNTS_MAX_AGE_SECS`, which is what heals a bump the emitter logged and
swallowed.

The dependency walk is generated once, by
`graph_sql::dependency_closure_cte`, and shared by the failure cascades, the
per-evaluation sweeps and the GC keep-set.

A consistency sweep (`graph_consistency_report`, interval
`GRADIENT_GRAPH_CONSISTENCY_INTERVAL`, default 300s) is the only backstop for the
counters, because both of them are moved rather than derived and nothing else
would ever notice a lost move. It repairs `cached_path.missing_references` over
the paths the pending anchors gate on, then recomputes `fetchable` and
`unready_deps` over the pending anchors and their direct dependencies, writes
what differs, settles the queue against the gates in both directions, names for
the live evaluations the pending anchors they reach through builders that nobody
names any more (`adopted`, a repair like the drift counts), and logs
what it repaired next to the two read-only alarms: terminal-success producers
with an unbacked output, and `Building` evaluations with no non-terminal anchor
left. The NAR repair runs first because the readiness recount reads wholeness,
so a drifted path would otherwise teach the anchors a count this very pass fixes.

Each chunk of either repair is its own transaction that takes the same ordered
`FOR UPDATE` pass a retire takes and only then recounts, so the recount's
snapshot opens after any commit of those rows has finished; a compare-and-swap on
the counted value alone is not enough, because a commit that reseeds a row onto
the drifted value passes it and is overwritten with a stale count. Chunking means
each chunk commits on its own instead of the sweep's budget rolling every repair
back. Neither repair iterates: a chunk recounts every row from one snapshot
and ripples nothing, so a chain of drifted rows converges one level per interval
and a row outside the repaired scope never does. Setting the interval to 0
disables the sweep, and with it the counters' only backstop and the graph-stuck
re-heal, which is its own pass on the same interval rather than part of the
report.

Three numbers on that line are not violations. The two drift counts report rows
the same pass already repaired, so a warning naming only those is a successful
self-repair, not a dead zone. `gating` and `repair_scope` are the sizes of the two
scope selects, neither of them bounded, logged on every sweep - clean or not - to
say what the recurring cost this pass adds actually is on a production graph: the
readiness scope is the smaller set and the more expensive one, since every row in
it is taken `FOR UPDATE` twice against rows every live graph writer also locks.
What the sweep does count table-wide is `negative_reference_counters`, rows whose
counter a ripple drove below zero: no gate can read such a row as whole again, and
the repair only rescues one that a pending anchor gates on, so outside that set it
is the one state the design calls unrecoverable - and the drift count is zero for
those rows by construction.

An evaluation in `EvaluatingFlake` or `EvaluatingDerivation` has one exit: the
`EvalStreamCompleted` / `EvalFailed` transition the scheduler sends once, when
the worker reports its job terminal. That message is droppable - both handlers
swallow a job the tracker has forgotten, and the graph call can time out or lose
its mailbox on an actor restart - and nothing else re-drives it, since the
waiting-state sweep leaves a pre-build evaluation alone whenever an eval-capable
worker is connected and `recover_interrupted_work` runs only at startup. The
`eval-completion-watchdog` pass (60s) closes that dead zone: it finds
evaluations whose newest eval job already carries a `finished_at` yet have not
been written for 900s, confirms the scheduler holds no job for them, and re-sends
the transition. The grace sits above the graph actor's 600s RPC timeout so a slow
transition is never mistaken for a lost one, and `EvalStreamCompleted` is
idempotent, so re-driving one that did land changes nothing.

The dispatch record is the one proof that a job is out. The `dispatched_job` row
is written inside `RequestJob`, before the assignment is handed back to the
session, so no report can precede its own row; a claim whose record cannot be
written is released to pending and the worker asks again; a record that is
written but whose anchor transition then fails is closed as `Abandoned` on the
way out, so a withdrawn claim never leaves an open row shutting the gate. A
build's `Dispatched` transition is awaited on that same path, but the open
`build_attempt` and the anchor's `dispatched_at` it writes are warn-only inside
it, so the row is what gates the hand-out and those two are not. That await is
capped at half the heartbeat deadline: the session handles one frame at a time,
so a graph actor merely queued behind an ingest burst would otherwise hold the
worker's next heartbeat unread past `worker_heartbeat_timeout_secs` and get a
healthy worker unregistered mid-assignment. Expiry takes the same withdrawal
path as a failure, with one thing it cannot undo - a transition that lands late
still spends the anchor's one-and-only `dispatched_at`. Both dispatch
selections refuse work with an open row: `find_ready_anchors` and the
queued-evaluation select carry a `NOT EXISTS` over `dispatched_job` keyed on the
scheduler's job key (`build:<anchor>` / `eval:<evaluation>`, whose prefixes live
in `gradient_db::dispatch_record` next to the SQL that rebuilds them), which is
what stops a core rebuilt with an empty tracker, or a worker slow to report it
started, from being handed the same evaluation twice. The tracker's `untracked`
filter stays as the in-memory fast path; the row is the durable one.

The gate is a check-then-act, not an enforced invariant: the partial index it
reads is not unique, and two rows for one key are reachable today, because a
fetch-only completion re-enqueues `eval:<id>` while the fetch row's close is
still on a detached task and a worker re-adopts a job under a new dispatch id.
What makes it sound is that exactly one scheduler core writes: `assign_pending`
serialises every claim in the actor, so the window between the select and the
insert is never open to a second claimer.

Six paths close a row. The worker's own terminal report stamps `finished_at`
and the outcome, matched on the dispatch id the report carries; a report whose
dispatch has no row at all is dropped with a warning, while one that arrives
after another closer got there first leaves the recorded outcome alone and still
lands its phase timeline. A worker that vanishes has its rows closed as
`Abandoned` by `requeue_orphaned_jobs`. A worker that rejects the assignment -
draining or at capacity, both routine - has that one row closed as it goes back
to pending, because a rejected job is not out and the sweep never reaps a row
the tracker still holds. Startup recovery closes every open row there is: it
already asserts that nothing the previous process handed out is still out, and
the anchors it re-queues in the same pass are refused by their own gate until
the rows go. Registration then closes what recovery missed, but only rows
dispatched before this process started: a row this process handed out still has
a closer in the terminal report landing for it, and a deploy reconnects the
worker inside exactly that window, so a blanket close by worker id would rewrite
a `Completed` dispatch as `Abandoned` and lose its eval phase totals. Rows whose
worker never returns are closed last by the `abandoned-dispatch-sweep` pass
(60s) 1800s after dispatch, provided the tracker no longer knows the job; with
recovery closing the restart case, that grace is a backstop rather than the
primary path. The tracker, not the clock, decides: a row it still holds is never
swept, however long the build runs.

`Abandoned` is deliberately distinct from `Failed`. The worker never reported,
so the build may well have succeeded before contact was lost; recording it as a
failure would feed invented failures into the board's rates and into
history-based scoring.

The last of the four promotion gates is the anchor's own `.drv` being whole in
the cache (`graph_sql::drv_whole_predicate`). A build worker cannot import a build target's
`.drv` until the `.drv`'s full reference closure (every transitive input `.drv`
plus its input sources) is in the cache - the daemon's `add_to_store_nar` rejects
a NAR with absent references. The eval pushes those `.drv`s progressively, so
without this gate a build dispatched mid-push fails terminal `InputsUnavailable`
on a missing `.drv` (its own or a dependency's), the dominant failure of large
NixOS system-closure derivations.

That gate is one integer on the `.drv`'s own `cached_path` row, not a mirror of it
on the anchor. A `.drv` is an ordinary store path whose references are exactly its
input `.drv`s and its `inputSrcs`, so `missing_references = 0` on a backed `.drv`
row already means the whole importable closure is there, input sources included -
which is why nothing gates on `derivation_input_source` any more. A build-graph
mirror of the same fact could only diverge from the NAR ground truth when eval
pruning leaves a dependency unwalked, and would then dead-zone a build whose
`.drv` closure is in fact fully cached. A substitutable anchor substitutes its
output and never imports its `.drv`, so the gate skips it; `inputSrcs` are still
recorded per derivation in `derivation_input_source` (parsed from the `.drv` at
`report_eval_result`) because the worker's prefetch pushes them, and because they
have no producing derivation that could re-push them later.

Because the anchor is global and build-once, a new evaluation is treated as a
fresh build intent: the eval-scoped reconcile thaws every anchor a previous eval
left terminal-failed across the evaluation's closure
(`requeue_failed_closure_for_eval`), and the substitute-miss budget is scoped
per evaluation. A permanent failure (or an exhausted substitute budget)
therefore does not poison every later evaluation that needs the derivation - the
world (upstream cache, network) may have changed since it failed.

Only a *genuine* miss counts toward the substitute-miss budget. The worker reports
`SubstituteUnavailable` (escalation-eligible) only when an output or a member of its
runtime closure is on no upstream; a transient relay failure - the Pull RPC timing
out, the NAR download, or the presigned PUT into our own store - is reported as a
retryable `Transient` instead.
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
`external_url` set records an upstream offer for a copy that is gone, so
`is_cached_anywhere` stays true and the reset-to-build anchor dead-ends on a `.drv`
nothing serves. The re-walk is a separate decision and belongs to `walked` alone:
the recovery paths that need one clear the bit explicitly, because a demote on its
own no longer makes a node prune-ineligible.

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
treats `InputsUnavailable` like a transient failure). Demoting the input's
producer raised the re-queued build's `unready_deps`, so it is pulled out of the
queue by the same statement and only promoted again once the rebuilt input is
fetchable - without the failure leaking onto a sibling evaluation that shares the
global anchor. The self-heal circuit breaker (`inputs_unavailable_max_loops`)
still caps the loop: once it trips, the input is deemed unrecoverable and the
build fails `Permanent` (then cascades), so a genuinely missing input can't retry
forever.

The cache can break the invariant from the other direction: an artifact that goes
without the retire that would have moved the anchor - a row deleted by hand, an
output a partial completion never backed at all - leaves the producer at
`Completed`/`Substituted` with nothing to serve. The gate
then trusts it, dependents fail `InputsUnavailable` permanently, and - being
terminal-*success*, not terminal-failed - it is never re-queued, so it never
rebuilds. `demote_unbacked_trusted_outputs` restores the row-vs-object invariant:
it finds every terminal-success producer (`status IN (3, 7)`) with **any** output
that is neither in our cache (a `cached_path` with a NAR) nor on an upstream
(`external_url`) and demotes it back to `Created`. It keys on the **ground truth**
(a missing backing NAR), **not** the derived `is_cached` flag: `is_cached` is
`false` whenever an anchor was marked `Completed` with an output that was *never* cached -
a partial cache-hit or substitution that set the anchor done without backing every
output (observed on multi-output CUDA derivations whose `out` was never pushed, no
build attempt). An `is_cached`-gated predicate skipped that case, stranding the
producer and its whole dependent subtree. The completion path records each output's
`cached_path` before flipping the anchor terminal (#303/#399), so a
genuinely-complete anchor is never demoted mid-completion. It runs hourly in the
cache loop (after the GC passes) and inside the reconciler's `Unstick` scope, so an
orphaned or partially-cached producer heals promptly - even while the evaluation
that needs it is itself stuck `Building` - without manual intervention.

The demote is a bet that a rebuild lands the artifact, and it is placed **once**.
The heal splits the invariant into two disjoint halves on whether the fleet has
already finished a real build of the producer (a `build_attempt` with
`substitute = false` and a successful outcome; a relay attempt never counts, and
the demote clears `substitutable` so the retry it grants is a real build):

- **not yet** - demote, as above. That is the one rebuild the anchor gets.
- **already** - the rebuild came and went and the output is still unbacked, so
  rebuilding does not restore it. The producer is marked `FailedPermanent` and the
  attempt that reported success is rewritten
  `Failed`/`OutputMissing` with a message naming the
  output. Its dependents then cascade `DependencyFailed` instead of counting it
  unready forever, and because `OutputMissing` is a *deterministic* failure a new
  evaluation does not thaw it into a rebuild that reproduces it. The artifact
  appearing is what recovers the anchor, through `reconcile_cached_anchors_for_eval`.

Without that split the heal has no memory and no verdict: it re-derives the same
demote every pass and the anchor runs `demote -> promote -> rebuild -> demote`
forever, one dispatch per pass, leaving a zombie `cached_path` behind each time,
while its dependents stay blocked regardless - `fetchable` wants every output whole
(#654).

The verdict waits out `narUploadGraceHours`, measured from the attempt's
`build_finished_at`. `JobCompleted` rides the control writer lane and overtakes its
own trailing `NarUploaded` commits, so an output is legitimately unbacked for a
while after its build reports success; that setting is the bound this system
already uses for exactly that window (the orphan-files GC and the uploader's
absent-row demote both measure against it). Before it, the anchor is in neither
half and the heal simply waits.

GC deletion also maintains the dispatch-gate invariant inline instead of leaving
it to a later sweep: every pass that deletes `cached_path` rows
(orphan-derivation GC, zombie purge, stale-path eviction, path invalidation) goes through
`nar_closure::retire_paths`, which in the **same transaction** raises the
`missing_references` counter of every referrer that trusted the deleted rows and
moves the anchor side of every hash it deleted, every hash that stopped being
whole, and every hash the caller asked it to retire: those producers lose
`fetchable` and their dependents' `unready_deps` rises, and the owner of a `.drv`
that is gone leaves the queue. One statement in that pass is deliberately narrower.
A terminal-success producer becomes a fresh build intent (recounted before it
re-enters the queue) only when its artifact is actually GONE - a row this pass
deleted, or a hash it was asked about that had none. A referrer that merely lost
wholeness still has its own output in the cache, so it needs `fetchable` to drop
until the missing path returns and nothing more; the forward ripple marks it
fetchable again then, which needs the terminal status a reset would have removed.
Resetting the referrer closure instead rebuilds artifacts that never went missing:
one deleted NAR re-queued 107 derivations and dispatched 139 builds in 30 s.
So there is no window in which the gate trusts an artifact GC just removed. None of
that reads the counter, so binding it to what MOVED would leave two rows behind: a
`.drv` deleted while it was not whole, and a hash with no `cached_path` row at all,
which deletes nothing and ripples nothing yet is half of what the unbacked-output
sweep matches. Every statement in that pass is ground-truth keyed, so widening the
set moves nothing extra. A caller
that may only drop a path while some condition still holds hands that condition to the retiring DELETE through
`retire_paths_where` rather than deciding it in a statement of its own, which would
decide from its own snapshot and cascade away a signature another cache wrote
meanwhile.

The guard alone is not enough either: a statement's snapshot is taken before it
blocks on the RI `FOR KEY SHARE` lock a concurrent `cached_path_signature` insert
holds, so the DELETE would cascade away a signature that committed while it waited
(the READ COMMITTED / EvalPlanQual argument behind that is written out once, in
`gradient_db::nar_closure`'s module doc, and three things here depend on it). The
wait is therefore absorbed by a preceding pure `FOR UPDATE` pass, hash-ordered, and
the DELETE then opens a fresh snapshot that sees the signature its guard tests. That
is why the guarded entry point takes a `&DatabaseTransaction` while the unguarded
`retire_paths` takes any connection: the lock has to still be held when the DELETE
runs, and on a pooled connection every statement is its own implicit transaction, so
a pooled guarded retire does not compile. Path invalidation goes further and demotes the producer itself
(`demote_cached_output`), so an invalidated output rebuilds instead of staying
trusted-but-gone. And because the per-task
evaluation GC refuses to run while any evaluation is active, a wedged `Building`
evaluation used to freeze a task's GC forever - an "active" evaluation whose
current phase has lasted longer than `gc_wedged_eval_hours` (default 24h) now
stops blocking, while never being deleted itself. The age is taken from the
phase stamps, not from `updated_at`: a wedged run still takes writes, so it
never looks untouched, and measuring the row's last write left the escape
hatch firing only for a run that had gone completely silent.

The deeper cause of those orphans is the derivation GC itself. `build_job` rows are
per-evaluation and pruned with old evals (`keep_evaluations`), but the global
`derivation_dependency` graph and the build-once anchors persist. The old orphan
pass treated "no `build_job`" as "unreferenced" and deleted the derivation (its
`.drv` + output NARs + `cached_path` rows), so a derivation still needed as a build
input of a retained closure - its own evals long gone - got swept away, stranding
dependents on `InputsUnavailable`. `gc_orphan_derivations` is now a mark-and-sweep:
it reclaims a derivation only when it lies *outside the build-dependency closure of
every live root* (`entry_point` ∪ `build_job` derivations, walked over
`derivation_dependency`). The per-task evaluation GC pairs with it from the other
side: a `build_job` is how an evaluation names what it waits on and deleting an
evaluation cascades its names, so before that GC settles the queue the live
evaluations adopt the pending anchors they reach through builders
(`reachability::adopt_pending_closures`), which is what keeps a pruned interior
queued, attributable and built once its walker is gone (#663). The orphan-NAR keep-set (`active_hashes`) likewise pins
the input sources and `.drv` hashes of every derivation with a build anchor - not
just outputs, and *regardless of build status*. These are producerless (only an eval
re-pushes them), so a terminal-failed anchor a later eval requeues must still find
its `.drv`; gating that clause on status purged the `.drv` of a failed-but-requeueable
build and dead-ended its retry on `InputsUnavailable`. Outputs stay status-gated
(they are rebuildable, and the eviction pass below reclaims them). Because attempts now outlive their
evaluations (set-null'd onto the anchor), the same pass also deletes the
`log_storage` files of every reclaimed derivation's attempts - the DB rows cascade,
but the log objects live outside the database, so they are reclaimed by hand like
the NARs. `pass_logs` in the deep GC is the backstop for any log object left behind.

The same reachability is the cache's keep-set. A cached path is live while the NAR
reference closure of a reachable derivation's outputs or `.drv` contains it; the
eviction pass (`evict_stale_cached_paths`, every cache-maintenance tick) removes
every path outside that set whose last fetch or commit is older than `cacheTtlHours`,
retiring the row through `retire_paths` so the closure counters and any producer's
`fetchable` follow. Derivation rows outside the reachable set are collected
separately after `keepOrphanDerivationsHours`; nothing else reclaims a NAR.

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

Evaluation GC (`gc_task_evaluations`) deletes old evaluations and relies on FK
cascade to clear their per-eval rows: `evaluation -> build_job -> build_attempt`.
`build_log_chunk` previously carried a bare `build_attempt` UUID with no FK, so its
chunk-index rows leaked forever once the eval (and its attempts) were collected; it
now cascades from `build_attempt`, completing the chain. The log blob itself is still
removed explicitly (it is object storage, not FK-tracked). `dispatched_job` and the
metrics firehose (`phase_event`, `worker_sample`, `metric_rollup`) are pruned by age
in the separate retention loop instead.

#### Upstream substitutability

A proxied NAR URL names its upstream by row id, but an upstream is configuration
that can be removed. Since a client caches narinfo and never refetches it, giving
up when that row is gone would strand the path forever, so the remaining
upstreams are tried too - the NAR path is content-addressed, so any upstream that
serves it serves the same bytes.

A narinfo proxied from an upstream is re-signed with the cache's own key before
it is served, so a client trusts one key - the Gradient cache's - rather than the
key of every cache it happens to proxy. The upstream's own signature is verified
first and kept alongside ours: re-signing something unverified would launder it
under our name, and a signature covers the fingerprint rather than the rewritten
`URL:`, so the upstream's stays valid for anyone who wants to check it.

An upstream that stops answering is taken out of rotation rather than probed
again on every request. Each probe is bounded at two seconds, and three
consecutive *transport* failures in a row trip that upstream for a minute, after
which one request is let through to test it - failing again re-trips it, so a
cache that is down costs nothing while it stays down and is picked up on its own
when it returns. A 404 is not a failure: it means the upstream answered and
simply lacks the path, which is the ordinary case during any large substitution.
The same health is shared by the worker cache-query path and the cache's own
narinfo endpoint, so a dead upstream is learned about once.

A derivation is just another build that can be substituted when its output is
available on a cache, exactly like any other - fixed-output derivations are not
special-cased. At eval time `resolve_anchors` runs a project-scoped lookup
(`compute_upstream_substitutable`): for every derivation not already in the
gradient cache it probes each output's `.narinfo` across the project's configured
upstream caches. A derivation is marked substitutable only when *every* one of
its outputs is cached somewhere (the gradient cache or an upstream); otherwise it
is built. The resolved upstream NAR URL plus narinfo metadata is persisted once
onto `derivation_output` (`external_url`, `nar_hash`, `file_size`,
`references_list`, `deriver`), so the narinfo lookup runs only once.

A substitutable anchor dispatches as a relay job on any worker once something
demands it (see [Promotion](#promotion)). The dispatch carries the derivation's
output `(name, store_path)` pairs in the `BuildSpec` so the worker fetches the
outputs directly and never touches the `.drv`: a substitution needs only the
output NAR plus its runtime closure, never the `.drv`'s build-time
`input_sources` (binary caches do not serve those, so importing the `.drv` would
fail with a spurious `SubstituteUnavailable`). The worker walks the upstream
references breadth-first from the outputs and pushes every member our cache
lacks - relaying each NAR verbatim when it is already zstd-compressed at our
2 MiB level-6 window, else recompressing - so the outputs land whole
(`missing_references = 0`) and the binary-cache invariant holds for substituted
anchors exactly as for built ones. Relaying the outputs alone is what used to
break it: the closure members below a pruned node have no anchor of their own, so
nothing ever fetched them and every dependent's build fell back to the upstream.
`use_substitutes` stays off in the daemon - substitution always goes through
gradient, never the worker's own nix config. Existing build-once anchors a prior
eval left not-yet-succeeded are flipped substitutable when an upstream is newly
found, so a previously-failed fetcher substitutes instead of rebuilding; the
anchors that flipped stop being builders, so what they demanded is released.

A `SubstituteUnavailable` miss re-queues the relay penalty-free. At
`substituteMissEscalationThreshold` misses within one evaluation the graph actor
exhausts the substitution instead: `substitutable` is cleared, the outputs forget
their upstream columns, and the anchor goes back to `Created` to be built through
the ordinary gates. That is a graph transition rather than a dispatch mode, so the
dispatcher reads the flag and nothing else - it used to escalate a
still-substitutable anchor to a real build and then stall it forever when no
worker of its architecture was connected. An unwalked stub waits for the next
evaluation, which no longer prunes it as upstream-served because nothing is
recorded on its outputs any more.

Upstream object fetches use a redirect-following HTTP client, separate from the
client that carries API traffic (forges, OIDC), which refuses redirects so a 3xx
cannot become an SSRF pivot. Attic, Cachix and every S3 gateway answer a NAR GET
with a 3xx to their object storage, and an unfollowed one arrives as an ordinary
response with an empty body. A download is accepted only when its length matches
the `file_size` the narinfo declared; anything else is an upstream that cannot
deliver the path, reported as `InputsUnavailable` so the self-heal demotes it and
rebuilds. Compression is read from the payload's own magic bytes rather than the
URL extension, because upstreams disagree with themselves - attic serves
`Compression: zstd` under a `nar/<hash>.nar` URL.

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

The eval closure walk prunes on the record alone. As the worker walks the graph it
asks the server which dependency derivations it already knows
(`QueryKnownDerivations`); the server prunes exactly those whose
`derivation.walked` is true. `walked` says the subtree is recorded - its outputs,
every declared dependency edge, every input source - which is the whole contract of
the walk, so that bit is the one fact which makes skipping the subtree lose nothing.
Build and cache state carry no such guarantee: keying the prune on an upstream
`external_url` or on whole outputs behind a terminal-success anchor re-walked a
complete record for as long as its anchor had not succeeded, which is most of a
running evaluation's graph. A record is dropped only where an edge can have been
lost, and dropping it is what makes the next eval walk the node again: the
missing-input self-heal flips `walked` on the referrers it demotes (below), and the
GC's orphan reclaim flips it on the survivors of a deleted dependency. A node that
is pruned can still be fetched on demand from an upstream cache that serves its
closure whole.

A stub is never pruned: `walked = false` means the subtree was never recorded.

#### The cache closure invariant

The cache holds a binary-cache invariant: *if an output is in our cache, its
entire runtime closure is too*. A build (and a substitution, which fetches the
output's closure locally first) pushes the **full runtime closure** of its
outputs, not just the output paths; already-cached members are skipped, so only
paths the cache is actually missing upload. Each upload's bytes are written to
storage by a tracked task per NAR, and its metadata is committed by the graph
actor's `CommitNar` message; the per-connection commit semaphore that used to
serialise two commits per session is gone, because the actor already serialises
every write to the index.

The dispatch gate does not merely *trust* this - it **enforces** it. A build's
build-time dependency edges (`derivation_dependency`) do not include a dep's
transitive runtime references, so "dep is Completed/Substituted" alone does not
guarantee the dep's runtime closure is fetchable. A dependent dispatched on that
weaker signal fails `InputsUnavailable` on a runtime path the gate never checked
(e.g. `nixos-system` needs `unit-bird.service` via `system-units`, which has no
direct edge). So completeness is tracked explicitly:

`derivation_build.fetchable` is the anchor-side reading of that invariant (see
[Promotion](#promotion)): an anchor is fetchable once every one of its outputs is
whole in our cache. A build's runtime references are a subset of its build inputs,
so an output whose own reference closure is whole is everything a dependent's
dispatch needs from it - closing the runtime-vs-build edge gap without a runtime
walk. Its dependents' `unready_deps` is moved from its flips, so a dependent that
finished before its dependency did needs no re-derivation of its own readiness:
the dependency's flip writes it.

A substituted anchor's outputs are whole the moment its relay finishes, so it is
fetchable like a built one. Its relay job carries no `required_paths`, so the
worker pulls no build deps and the job scores a uniform zero. The gate itself
stays a single integer comparison, and partial indexes on `derivation_build` keep
both scans off the full anchor table: the dispatch queue matches `status = Queued`
in `updated_at` order, and the table-wide promote matches
`status = Created AND (unready_deps = 0 OR substitutable)`.

The NAR side of that invariant is a counter, not a flag.
`cached_path.missing_references` is the number of a path's references (self
excluded) whose row is absent, unbacked or itself not whole; a backed row with
`missing_references = 0` is *whole*, and `gradient_db::nar_closure::whole_predicate`
is the one definition every gate reads. The graph actor seeds the counter when it
commits a NAR, from the references the worker reported, and when that flips the path
to whole it decrements every referrer, then every referrer of the referrers that
just reached zero, one statement per level. Deleting a row (`retire_paths`: the
orphan GC, the zombie purge, the stale-path eviction, every demote) runs the same ripple in
reverse from the rows that were whole, and moves the anchor side of what those rows
backed in the same transaction. Every ripple is driven by a **transition**, never by a
state: rippling from a row that did not just flip moves its referrers past zero, and
a negative counter never satisfies `= 0` again.

The graph actor handles one message at a time, so a commit never overlaps another
commit or one of its own demotes. Three of those deletions do run outside it, each
in its own transaction - the stale-path eviction, the zombie purge and the orphan GC - and
nothing serialises them against a commit but row locks. A commit therefore locks its
reference endpoints first: the references it reports, the ones already indexed for
it, and its own row, in one hash-ordered `FOR SHARE` statement taken before the row
it is about to write. That leaves no window in which a retire and a commit disagree
about an edge - the retire's `DELETE` waits for the commit, and its reverse ripple, a
statement of its own, then counts the new edge - and none in which a retire deeper in
the closure unwholes one of those references between the seed's read and the commit,
which is why the lock is `FOR SHARE` and not the weaker `FOR KEY SHARE` a foreign-key
check would take: only `FOR SHARE` conflicts with a ripple's non-key `UPDATE`.

Every writer that takes row locks here takes them in one statement ordered by hash, a
retiring `DELETE` behind its own `FOR UPDATE` pass included, because a single
unordered locker deadlocks against however carefully ordered the other side is. The
ripples are the exception and it is deliberate: each computes its referrer set inside
its own `UPDATE`, so it locks rows no ordered set covers, in plan order. A commit and
a concurrent maintenance retire can therefore still deadlock; Postgres detects it, the
retire retries on its next pass and a killed commit fails a `NarUploaded` the worker
retries. A detected, retried deadlock is the accepted price of never leaving a row
whole with a reference that is not. Nothing re-derives the counter by a
sweep; the consistency pass above recomputes it only for the paths pending anchors
gate on and repairs what disagrees, and is its only backstop.
The migration that introduced the counter converges the cache's old closure flag
one last time, seeds the counter from it and drops it, so the first start after
the upgrade reads a sound value - on a large cache that runs for minutes.

A build's runtime references are a subset of its build inputs, so a whole output
closure is what a dependent's dispatch needs from that output, and a whole `.drv`
row (its references are exactly the input `.drv`s and input sources) is what a
build target's import needs. The dispatch gate reads both through `whole`.

The repair is bounded to the gating paths - the pending anchors' own `.drv` rows
and the output rows of their direct dependencies - and that is narrower than the
readers. The substitutability pass asks whether the outputs of arbitrary walked
candidates are whole, and most of those have no pending anchor, so a ripple lost
there is never recomputed. It is also the worse failure: a false-whole there flags a
derivation as upstream-substitutable that nothing serves, so the subtree is never
built - a permanent dead end rather than a stall a later build clears. Widening the
recompute past the gating set is still open. The eval-time prune is no longer one of
those readers: it keys on `derivation.walked` and never asks whether a path is whole.

When a build still reports a path missing, `reconcile_missing_inputs` self-heals: a
missing leaf with a producer is purged and rebuilt (`demote_cached_output`, which
retires the row and ripples the loss to every referrer so dependents re-block until
the leaf re-pushes whole); a producerless source demotes its direct **referrers**
(`demote_referrers_of`) so a referrer rebuild re-pushes it.

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
the cache self-correcting regardless of how a desync arose. The same premise
governs a path's **reference set**: an input-addressed path rebuilt
non-deterministically keeps its hash while its closure moves, so the commit
rewrites `cached_path_reference` to exactly the set the worker reported instead
of adding to it: one statement that prunes the edges the report no longer carries
and re-positions the survivors, since dropping one reference shifts every later
one and `position` is what the narinfo `References:` line and the signature
fingerprint are reconstructed from. An edge an add-only write left behind would
stay counted in `missing_references` forever, and the consistency sweep's repair
recomputes from that same table, so it could never disagree with the stale row.

An **orphan producer** is the third case: the missing leaf has a producing
derivation, but that producer has no `build_job` (it was pruned out of the build
graph because a referrer's output was cached without its closure under output-only
substitution), so promotion can never queue it and the gentle flag clear leaves
the referrer cached, pruned, and never re-walked. When `demote_cached_output`'s
producer is not reachable (`derivation_is_reachable` is false), the referrers are
demoted (`demote_referrers_of`) and their record is dropped
(`unwalk_derivations`), so the next eval walks them again, re-records the dropped
edge, and schedules the orphan. The bit looks redundant next to the demote - the
`cached_path` row is gone, so the node is uncached - but cache presence is not what
the prune reads any more, so the re-walk has to be asked for explicitly.

An **absent orphan** is the fourth case and the one that makes the whole thing
self-heal without operator surgery: the missing input has *no* producer row and
*no* indexed referrer (it was pruned out so thoroughly it was never recorded, or
an admin deleted its rows), so it cannot be reached upward at all. Instead it is
reached downward from the known failing build: `demote_output_only_cached_deps`
demotes that build's output-only-cached direct dependencies (output present in our
cache, no `external_url`) and drops their record, forcing the next eval to walk them
again and re-record the orphan plus its now-buildable subtree. Upstream-fetchable
deps (`external_url`) are
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
`/graph`) accept requests from members of any project whose evaluation
references the derivation (a `build_job` exists for it in one of that project's
evaluations). Garbage collection reads reachability
differently: a derivation is reclaimed only when it lies outside the
build-dependency closure of every `entry_point` and `build_job` root, not when it
merely has no `build_job` of its own.

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
  worker can satisfy any pending build. Pending means what still blocks the
  evaluation, so an anchor nothing demands is not in the set, and an evaluation
  with nothing left in it is finalized here rather than parked.
- **Graph stuck** - the pool *can* build every pending anchor (so the `workers`
  reason would carry an empty `unmet` set) yet none is dispatchable: nothing in
  the pending set passes the dispatch gate and no in-flight build is left to fire
  a promotion. What blocks it is not recorded on the reason; `pending_anchors` is
  the blocked count. The reconciler
  detects it and self-heals in six steps: `requeue_failed_closure_for_eval`
  thaws any terminal-failed anchor in the eval's full dependency closure (a
  transitive dep a prior eval left failed and this eval pruned has no `build_job`
  here, so `requeue_failed_anchors` never reaches it and it blocks its dependents
  with no dispatch to fail); `demote_unbacked_trusted_outputs` resets a producer
  the graph trusts against an artifact nothing serves; `reconcile_cached_anchors_for_eval`
  marks every anchor in the closure whose outputs are all in our cache `Completed`
  (build-graph state desyncs from the durable cache state - a derivation whose
  artifacts exist sits `Created` after a requeue/cascade/demote and blocks its
  dependents, so cache presence is trusted as the ground truth) and advances the
  dependents of what it settled; then the dependency-failed sweep over the closure;
  then it names for the evaluation every pending anchor it reaches through
  builders and nobody names any more (`adopt_pending_closure`, a pruned interior
  whose walker was deleted); then `promote_closure`. It re-assesses: recovers
  to `Building` when the heal frees an anchor, else parks `graph_stuck` (the blocked
  count). The heal runs on entry, again whenever `pending_anchors` changes, and
  otherwise on the consistency sweep's cadence, because
  `reconcile_cached_anchors_for_eval` and `reconcile_dependency_failed` have no
  other driver; between those the counters promote the evaluation the moment
  whatever it waits on arrives.

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

- Every open `dispatched_job` row is closed as `Abandoned`. Nothing the dead
  process handed out is still out, and both dispatch selections refuse a job
  whose row is open, so this runs before the requeue below: otherwise every
  anchor recovery re-queues stays gated until its worker reconnects - which a
  scaled-down or crashed one never does - or the 1800s sweep reaches it.
- Orphaned `Running` build attempts are marked `Aborted`.
- Every `Building` anchor is reset to `Queued`: the worker that was building it
  is gone. This is a blanket reset, not a re-dispatch decision - the eval sweep
  below still aborts the ones no live evaluation wants.
- Every active evaluation a restart loses is aborted, and its task gets
  `ForceEvaluation` so a fresh evaluation re-walks it and writes a complete
  graph. That set is `EvaluationStatus::ACTIVE` minus the two the scheduler
  re-drives on its own (`Queued`, re-offered by the eval dispatcher, and
  `Waiting`, picked up by build reconcile), so `Building` is in it: an
  evaluation that had finished walking is re-evaluated rather than resumed,
  because nothing else would drive its remaining builds.
- The anchors those aborted evaluations drove are aborted too
  (`Created`/`Queued`/`Building` -> `Aborted`), including the ones the
  `Building` reset just re-queued. This mirrors the explicit-abort path: the
  builder aborts the evaluation's builds when the server dies, so the server
  reflects it. A global build-once anchor a still-live evaluation also needs is
  left running (shared-anchor safety). The forced re-evaluation re-drives the
  aborted anchors - `requeue_failed_anchors` resets them to `Created` - and they
  promote once their derivations are walked.
