/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Dispatch records: open assignments, build attempts, clusters, priority and the
//! startable set the dispatcher reads.

pub mod assignment_record;
pub mod build_attempt;
pub mod build_watchdog;
pub mod cluster;
pub mod priority;
pub mod startable_set;
