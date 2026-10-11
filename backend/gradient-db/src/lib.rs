/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod access;
pub mod actions;
pub mod build_jobs;
pub mod build_products;
pub mod build_request_task;
pub mod cache_paths;
pub mod cache_subscription_requests;
pub mod cached_paths;
pub mod caches;
pub mod chunked;
pub mod commits;
pub mod connection;
pub mod context;
pub mod dashboard;
pub mod deliveries;
pub mod derivation_builds;
pub mod derivation_outputs;
pub mod entry_points;
pub mod error;
pub mod evaluations;
pub mod flake_input_overrides;
pub mod graph;
pub mod integrations;
pub mod lookup;
pub mod maintenance;
pub mod memberships;
pub mod metrics;
pub mod pool;
pub mod projects;
pub mod roles;
pub mod rows;
pub mod scheduling;
pub mod sql;
pub mod status;
pub mod task_board;
pub mod tasks;
pub mod teams;
pub mod triggers;
pub mod users;
pub mod worker_registrations;

#[cfg(test)]
pub(crate) mod test_ctx;

pub use self::chunked::{IN_CHUNK_SIZE, fetch_in_chunks, for_each_chunk};
pub use self::context::{DbContext, DbMemory, HeldEvaluations, Hold, ProbeRequests, Wakeups};
pub use self::error::MissingRow;
pub use self::pool::{CacheDb, WebDb, WorkerDb};
