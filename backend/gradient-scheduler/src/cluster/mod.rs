/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Cluster jobs on the scheduler side: members wait in the book until their
//! cluster is whole, and idle slots are the planner's only capacity view.

pub(crate) mod book;
mod divert;
mod matching;
mod planner;
mod slots;

pub use book::{ClusterBook, ClusterMember, PendingCluster};
pub(crate) use divert::{Membership, Route};
pub use matching::kuhn;
pub use planner::{Placement, ScoreLookup, Seat, plan};
pub use slots::{ClusterSnapshot, IDLE_SLOT_TTL, IdleSlots, Slot, SlotKind};
