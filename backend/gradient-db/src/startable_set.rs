/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The shared builds that entered or left `Queued`, on their way to the scheduler's
//! startable set. Every status move already fans out through
//! [`crate::emit_transition_effects`], so the dispatcher reads what moved
//! instead of re-selecting every queued shared build per tick. A move is a hint, never
//! a claim: the dispatcher re-reads each shared build it is handed, and its periodic
//! resync covers a hint that was lost or that another instance produced.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use gradient_entity::build::BuildStatus;
use gradient_types::DerivationId;

use crate::status::TransitionChange;

type Waker = Box<dyn Fn() + Send + Sync>;

/// The net moves since the dispatcher last took them; a derivation sits in at
/// most one of the two sets, the one its latest move put it in.
#[derive(Debug, Default, PartialEq)]
pub struct StartableMoves {
    pub entered: HashSet<DerivationId>,
    pub left: HashSet<DerivationId>,
}

impl StartableMoves {
    fn enter(&mut self, derivation: DerivationId) {
        self.left.remove(&derivation);
        self.entered.insert(derivation);
    }

    fn leave(&mut self, derivation: DerivationId) {
        self.entered.remove(&derivation);
        self.left.insert(derivation);
    }

    fn record(&mut self, change: &TransitionChange) {
        match (change.from, change.to) {
            (BuildStatus::Queued, BuildStatus::Queued) => {}
            (_, BuildStatus::Queued) => self.enter(change.derivation),
            (BuildStatus::Queued, _) => self.leave(change.derivation),
            _ => {}
        }
    }

    fn absorb(&mut self, later: StartableMoves) {
        later.left.into_iter().for_each(|d| self.leave(d));
        later.entered.into_iter().for_each(|d| self.enter(d));
    }

    pub fn is_empty(&self) -> bool {
        self.entered.is_empty() && self.left.is_empty()
    }
}

/// A move made inside a transaction is staged until [`Self::publish`] after the
/// commit: the dispatcher reads on its own connection, and a shared build it looks
/// up before the promotion lands is not `Queued` yet and would wait out the
/// resync.
#[derive(Clone, Default)]
pub struct StartableSet {
    published: Arc<Mutex<StartableMoves>>,
    staged: Option<Arc<Mutex<StartableMoves>>>,
    waker: Arc<OnceLock<Waker>>,
}

impl std::fmt::Debug for StartableSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartableSet")
            .field("staged", &self.staged.is_some())
            .finish_non_exhaustive()
    }
}

impl StartableSet {
    /// Called on every published move; set once, by the dispatcher.
    pub fn on_move(&self, waker: impl Fn() + Send + Sync + 'static) {
        let _ = self.waker.set(Box::new(waker));
    }

    pub fn record(&self, changes: &[TransitionChange]) {
        self.update(|moves| changes.iter().for_each(|c| moves.record(c)));
    }

    /// Hand shared builds back to the dispatcher to be read again, such as one whose
    /// claim was lost to a gate that may have moved since it was assembled.
    pub fn enter(&self, derivations: impl IntoIterator<Item = DerivationId>) {
        self.update(|moves| derivations.into_iter().for_each(|d| moves.enter(d)));
    }

    pub fn take(&self) -> StartableMoves {
        std::mem::take(&mut *lock(&self.published))
    }

    /// The handle a transaction records into; a nested one shares its stage.
    pub fn staged(&self) -> Self {
        Self {
            staged: Some(self.staged.clone().unwrap_or_default()),
            ..self.clone()
        }
    }

    /// The handle for work past the transaction, which publishes at once.
    pub fn unstaged(&self) -> Self {
        Self {
            staged: None,
            ..self.clone()
        }
    }

    /// Hand the committed transaction's moves to the dispatcher.
    pub fn publish(&self) {
        let Some(staged) = &self.staged else {
            return;
        };

        let moves = std::mem::take(&mut *lock(staged));
        if !moves.is_empty() {
            lock(&self.published).absorb(moves);
            self.wake();
        }
    }

    fn update(&self, f: impl FnOnce(&mut StartableMoves)) {
        match &self.staged {
            Some(staged) => f(&mut lock(staged)),
            None => {
                f(&mut lock(&self.published));
                self.wake();
            }
        }
    }

    fn wake(&self) {
        if let Some(waker) = self.waker.get() {
            waker();
        }
    }
}

fn lock(moves: &Mutex<StartableMoves>) -> std::sync::MutexGuard<'_, StartableMoves> {
    moves
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn change(derivation: DerivationId, from: BuildStatus, to: BuildStatus) -> TransitionChange {
        TransitionChange {
            derivation,
            from,
            to,
        }
    }

    fn woken(set: &StartableSet) -> Arc<AtomicUsize> {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        set.on_move(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        wakes
    }

    #[test]
    fn only_moves_across_queued_count() {
        let set = StartableSet::default();
        let (promoted, assigned, unrelated) = (
            DerivationId::now_v7(),
            DerivationId::now_v7(),
            DerivationId::now_v7(),
        );

        set.record(&[
            change(promoted, BuildStatus::Created, BuildStatus::Queued),
            change(assigned, BuildStatus::Queued, BuildStatus::Building),
            change(unrelated, BuildStatus::Building, BuildStatus::Completed),
            TransitionChange::unchanged(unrelated, BuildStatus::Queued),
        ]);

        let moves = set.take();
        assert_eq!(moves.entered, HashSet::from([promoted]));
        assert_eq!(moves.left, HashSet::from([assigned]));
        assert!(set.take().is_empty(), "taking drains the moves");
    }

    #[test]
    fn the_latest_move_of_a_derivation_wins() {
        let set = StartableSet::default();
        let d = DerivationId::now_v7();

        set.record(&[
            change(d, BuildStatus::Created, BuildStatus::Queued),
            change(d, BuildStatus::Queued, BuildStatus::Created),
        ]);
        assert_eq!(set.take().left, HashSet::from([d]));

        set.record(&[change(d, BuildStatus::Queued, BuildStatus::Created)]);
        set.enter([d]);
        let moves = set.take();
        assert_eq!(moves.entered, HashSet::from([d]));
        assert!(moves.left.is_empty());
    }

    /// The dispatcher reads on its own connection, so a promotion it is told
    /// about before the commit reads as not `Queued` and is lost until the
    /// resync. Staged moves reach it only once published, and a rolled-back
    /// transaction publishes nothing.
    #[test]
    fn a_transactions_moves_wait_for_its_commit() {
        let set = StartableSet::default();
        let wakes = woken(&set);
        let d = DerivationId::now_v7();

        let tx = set.staged();
        tx.staged()
            .record(&[change(d, BuildStatus::Created, BuildStatus::Queued)]);
        assert!(set.take().is_empty());
        assert_eq!(wakes.load(Ordering::Relaxed), 0);

        tx.publish();
        assert_eq!(set.take().entered, HashSet::from([d]));
        assert_eq!(wakes.load(Ordering::Relaxed), 1);

        let rolled_back = set.staged();
        rolled_back.record(&[change(d, BuildStatus::Created, BuildStatus::Queued)]);
        drop(rolled_back);
        assert!(set.take().is_empty());
    }

    #[test]
    fn a_move_outside_a_transaction_wakes_the_assigner_at_once() {
        let set = StartableSet::default();
        let wakes = woken(&set);
        let d = DerivationId::now_v7();

        set.staged()
            .unstaged()
            .record(&[change(d, BuildStatus::Created, BuildStatus::Queued)]);

        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        assert_eq!(set.take().entered, HashSet::from([d]));
    }
}
