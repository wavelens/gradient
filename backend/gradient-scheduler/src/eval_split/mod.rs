/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod planner;
pub mod reply;

pub use reply::{MemberView, NextStep, SplitView, next_step};

pub const SPLIT_SUBPROCESSES: usize = 4;

pub fn split_params(max_members: usize) -> planner::SplitParams {
    planner::SplitParams {
        max_members,
        subprocesses: SPLIT_SUBPROCESSES,
        ..Default::default()
    }
}
