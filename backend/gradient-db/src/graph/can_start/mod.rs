/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `derivation_build.fetchable` and `derivation_build.blocking_deps`: whether a
//! shared build can serve its outputs to a parent, and how many of a shared build's direct
//! dependencies cannot. Zero is the can-start gate, so promotion and dispatch read
//! one integer per row where they used to walk the build graph, and nothing in this
//! module recurses over `derivation_dependency`.
//!
//! Every flip of `fetchable` is written by a statement whose `RETURNING` names
//! exactly the shared builds that changed, the direct parents' counter moves from that
//! set in ONE update, and the parents that reached zero are promoted.
//! [`crate::graph::predicates::fetchable_predicate`] is the one definition of the flag and
//! [`crate::graph::predicates::gates_predicate`] the one definition of the gate.
//!
//! Like `derivation_build.missing_runtime_deps`, this counter is MOVED and not derived, so
//! every ripple must be driven by a TRANSITION and never by a state: rippling from a
//! row that did not just flip, or rippling one frontier twice, moves a parent past
//! zero, and a negative counter never satisfies `= 0` again. [`became_fetchable`] and
//! [`lost_fetchability`] therefore mark first and ripple only from the rows their own
//! `RETURNING` reports. `NOT db.fetchable` (respectively `db.fetchable`) is a column
//! of the row being updated, so Postgres re-checks it under EvalPlanQual after any
//! lock wait, and two concurrent markers cannot both claim one flip.
//!
//! # One level, not a fixpoint
//!
//! The NAR ripple recurses because a complete closure is transitive: a path that becomes complete
//! makes its parent complete. The can-start state is not. A parent that reaches zero becomes
//! QUEUED, not fetchable - only a finished build or an upstream copy makes a shared build
//! fetchable - so the frontier stops at the direct parents and one statement per
//! flip is the entire ripple. Nothing here can change a shared build's own fetchability
//! either: both ripples write `blocking_deps`, and the only status they touch is a
//! move inside `{Created, Queued}`, which can neither enter nor leave the
//! terminal-success pair the predicate reads.
//!
//! # Every write executes under a lock proof
//!
//! A flip is a mark plus a compensating ripple, and they are separate statements. On
//! a pooled handle that is two implicit transactions: a ripple killed by a deadlock or
//! a statement timeout leaves the flip committed with its counter move gone, and
//! retrying the call is a silent NO-OP, because the mark no longer matches the row it
//! already flipped. So the pair is not merely better inside one transaction, it is
//! only correct there, and for [`lost_fetchability`] the lost move is the fail-open
//! direction: the parents keep `blocking_deps` too LOW, stay promotable, and
//! dispatch against an input nothing can provide.
//!
//! [`SharedBuildLock`] is how that is required rather than requested. [`lock_shared_builds`]
//! takes the shared builds `FOR NO KEY UPDATE` in one `derivation`-ordered statement and returns
//! the only proof [`became_fetchable`] and [`lost_fetchability`] accept, so neither can
//! run on a pooled handle, in another transaction, or over a row the lock did not
//! name. A retry is then a retry of the whole flip. The lock is `NO KEY` because a
//! `FOR UPDATE` would also hold every other transaction's foreign-key check on these
//! rows, a dispatch claim or a build report, until the flush commits.
//! [`seed_blocking_deps`] takes the
//! stronger [`SeedLock`], which also holds the dependencies it counts under shared
//! advisory keys while every flip holds its shared builds' keys exclusively
//! ([`crate::graph::shared_build_guard`]): an absolute count and a concurrent flip of what it
//! counts then cannot both miss each other's uncommitted rows.
//!
//! The RIPPLES are outside that discipline, deliberately, and this is the one thing
//! the proof does not cover: they write the flipped shared builds' PARENTS, which no
//! ordered lock names, in whatever order their plan produces. They move the counter
//! relative to the row's own value, so they compose with a concurrent move and need no
//! lock to be correct; what they can do is deadlock against another ripple or against
//! the repair's ordered pass. Postgres detects that rather than hanging, and because
//! the mark and the ripple now share a transaction the detection rolls the flip back
//! with them, which is what makes the caller's retry mean something.
//!
//! # What the repair covers, exactly
//!
//! [`can_start_scope`] materialises the scope once and the two repairs chunk it: per chunk, one
//! transaction takes [`lock_shared_builds`] and recounts only the rows that chunk names. The
//! lock is load-bearing, not hygiene. An UNLOCKED absolute recount reads its new value
//! from the statement's snapshot while its compare-and-swap reads the stored one from
//! the fresh row version, because EvalPlanQual re-checks only the target row's own
//! columns and never re-evaluates the predicate's subqueries; a concurrent change that
//! leaves the stored value equal to the snapshot's passes the swap and writes a value
//! that was already false. Measured on Postgres 18: a shared build stored
//! `fetchable = false` whose outputs the recount's snapshot saw complete, racing a retire
//! of the only output, ends stored `true` with a true value of `false`, and
//! `fetchable = true` is exactly what stops it counting toward its parents'
//! `blocking_deps`. Chunking is the other half: one transaction over the whole scope
//! holds `FOR NO KEY UPDATE` on every pending shared build while it works, and a statement timeout
//! or the sweep's budget then cancels it in place and rolls back every repair,
//! silently.
//!
//! Both `fetchable` recounts finish, across every chunk, before the first counter
//! recount starts. A counter computed from a `fetchable = true` that a later chunk was
//! about to correct is too LOW, and too low promotes.
//!
//! Each chunk locks what it WRITES, not what it READS. The counter recount reads
//! `dep.fetchable` for dependencies the chunk does not name, so a flip that commits
//! after the recount's snapshot is invisible to it while the compare-and-swap, which
//! only guards the target row, still passes: the stale count lands, and if the
//! dependency LOST fetchability the count is too low, which promotes. The flip's own
//! ripple writes the same parent, so whichever of the two commits second wins and
//! the next sweep converges. Closing it would need the lock to cover the transitive
//! read set, which is the unchunked pass this shape exists to avoid.
//! the complete-closure recount carries the identical residual for the same
//! reason.
//!
//! Repaired: both columns, over the pending shared builds and their direct dependencies as
//! of the scope select, plus `fetchable` wherever the stored flag contradicts a column
//! the row carries: `true` with a missing runtime dependency counted, or terminal success without
//! the flag. The complete-closure recount is table-wide and flips nothing, so without those
//! two a counter it raises two hops below anything pending left the flag `true` for
//! good, and a stale `true` is a settled shared build to every walk: 19 such rows stood
//! between 47 builders and the paths retention had taken. Written: `blocking_deps` for
//! every parent of a flipped shared build at ANY status and for every shared build a caller
//! seeds. What still has no backstop is `blocking_deps` on a `Building`,
//! `FailedTransient` or terminal row that no pending shared build depends on: both ripples
//! write it and no recount visits it, and a requeue thaws it back to `Created`, where
//! the very next promote pass can read the drifted value.
//!
//! A stale flag was worse than it reads, and is why [`seed_blocking_deps`] EVALUATES
//! [`crate::graph::predicates::fetchable_predicate`] on the dependencies it counts instead of
//! reading their `fetchable` column. The seed executes inside the record transaction the
//! moment a parent's edges land, so it is the first reader of a dependency's
//! can-start state and strictly precedes any sweep; reading a stale flag there seeds a fresh
//! parent to zero, promotes it and dispatches it, and the repair then heals the flag
//! after the build it caused. Evaluating the predicate costs a correlated subquery per
//! edge, once per record batch rather than once per tick, which is where that cost
//! belongs. Every other reader of `blocking_deps` is
//! [`crate::graph::predicates::gates_predicate`] on a `Created` or `Queued` row, which the
//! repair does cover.
//!
//! `blocking_deps` carries no `CHECK (blocking_deps >= 0)` and must not get one: a ripple
//! legitimately passes through intermediate values inside its own transaction, so the
//! constraint would abort correct work. `missing_runtime_deps` omits it for
//! the same reason.
//!
//! # What this module deliberately leaves to its callers
//!
//! A flip re-checks exactly one of the gate's four inputs. A shared build's own queue
//! membership is not a function of its own `fetchable`, so neither flip touches it:
//! [`lost_fetchability`] moves the PARENTS of the shared builds it flipped and never the
//! shared builds themselves. What it does re-open is the walk BELOW them: a shared build that
//! stopped being fetchable is open, what its outputs reference is wanted again, and
//! nothing else would ask, so it updates the need flag from the flipped rows. The other
//! three inputs each need their own call. A finished walk
//! setting `walked` is [`promote_closure`]; a `build_job` appearing, a `.drv` becoming
//! complete and the need arriving are [`promote`]; a `.drv` ceasing to be complete is
//! [`unpromote_drv_owners`]; the need going away is [`unpromote_ungated`] over what
//! [`update_need`] reports lost. `cache_available` being cleared on a `Queued` shared build is the
//! one with no entry point here, because it both unfetches the shared build and fails the
//! shared build's own gate: the caller that clears it owes that shared build a re-check of its own
//! gate, and until it does, [`repair_can_start`]'s un-promote pass settles it one sweep
//! later.
//!
//! `m20260908_000002` carries a frozen copy of
//! [`crate::graph::predicates::fetchable_predicate`] and of the gates, and there is
//! deliberately NO test asserting the two are equal. `gradient-db` depends on
//! `gradient-migration`, so such a test would compile; it would also be correct only
//! until the live predicate legitimately evolves, and would then fail for a good reason
//! while pressuring someone into editing a released migration, which must never happen.
//! That agreement is verified once, by review, at the commit that introduces both.

mod fetchable;
mod lock;
mod need;
mod queue;
mod repair;
#[cfg(test)]
mod test_rows;

pub use fetchable::{advance_fetchable, became_fetchable, lost_fetchability, seed_blocking_deps};
pub(crate) use lock::ids;
pub use lock::{SeedLock, SharedBuildLock, lock_seed_shared_builds, lock_shared_builds};
pub use need::{NeedMoved, SettledNeed, recount_wanted, settle_skipped, update_and_settle_need};
pub(crate) use need::{settle_need, update_need};
pub use queue::{
    promote, promote_closure, unpromote_drv_owners, unpromote_ungated, unwalk_derivations,
};
pub use repair::{Repaired, can_start_scope, repair_can_start, repair_fetchable};
