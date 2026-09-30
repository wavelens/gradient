/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Cluster jobs on the scheduler side: members wait in the book until their
//! cluster is whole, and idle slots are the planner's only capacity view.

pub(crate) mod book;
mod coordinator;
mod divert;
mod matching;
mod planner;
mod settlement;
mod slots;

pub use book::{ClusterBook, ClusterMember, PendingCluster};
pub use coordinator::*;
pub(crate) use divert::{Membership, Route};
pub use matching::kuhn;
pub use planner::{Placement, ScoreLookup, Seat, plan};
pub use settlement::*;
pub use slots::{ClusterSnapshot, IDLE_SLOT_TTL, IdleSlots, Slot, SlotKind};

#[derive(Debug, Clone)]
pub struct CommittedSeat {
    pub worker: String,
    pub key: String,
    pub job: crate::jobs::PendingJob,
    pub record: crate::jobs::DispatchRecord,
    pub role: String,
    pub index: u32,
    pub primary: bool,
    pub zone: Option<String>,
    pub endpoint: Option<String>,
}

/// A cluster taken from the book with its seats' members already active.
#[derive(Debug)]
pub struct Committing {
    pub cluster: PendingCluster,
    pub seats: Vec<CommittedSeat>,
}

/// A member's assignment waiting for its session to hand it out.
pub struct PreparedMember {
    pub assignment: crate::jobs::Assignment,
    pub membership: gradient_wire::types::ClusterMembership,
}
