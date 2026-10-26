/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod create;
pub mod planner;
pub mod recovery;
pub mod reply;
pub mod shrink;

pub use reply::{MemberView, NextStep, SplitView, next_step};

pub const SPLIT_SUBPROCESSES: usize = 4;

pub fn is_split_role(role: &str) -> bool {
    role == gradient_wire::types::SPLIT_PRIMARY_ROLE
        || role == gradient_wire::types::SPLIT_COMPANION_ROLE
}

pub fn split_params(max_members: usize) -> planner::SplitParams {
    planner::SplitParams {
        max_members,
        subprocesses: SPLIT_SUBPROCESSES,
        ..Default::default()
    }
}
