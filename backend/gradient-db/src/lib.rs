/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod access;
pub mod actions;
pub mod api_keys;
pub mod audit_log;
pub mod build_jobs;
pub mod build_log_chunks;
pub mod build_products;
pub mod build_request_blobs;
pub mod build_request_task;
pub mod cache_paths;
pub mod cache_subscription_requests;
pub mod cached_paths;
pub mod caches;
pub mod chunked;
pub mod cli_device_authorizations;
pub mod commits;
pub mod connection;
pub mod context;
pub mod dashboard;
pub mod deliveries;
pub mod derivation_builds;
pub mod derivation_dependencies;
pub mod derivation_outputs;
pub mod derivations;
pub mod entry_points;
pub mod error;
pub mod eval_cache;
pub mod eval_costs;
pub mod eval_shards;
pub mod evaluations;
pub mod flake_input_overrides;
pub mod flake_output_nodes;
pub mod graph;
pub mod integrations;
pub mod invitations;
pub mod lookup;
pub mod maintenance;
pub mod memberships;
pub mod metrics;
pub mod pool;
pub mod projects;
pub mod pull_request_summaries;
pub mod roles;
pub mod rows;
pub mod runner_jobs;
pub mod scheduling;
pub mod sessions;
pub mod sql;
pub mod ssh_keys;
pub mod status;
pub mod task_board;
pub mod tasks;
pub mod teams;
pub mod triggers;
pub mod upload_sessions;
pub mod users;
pub mod webhooks;
pub mod worker_names;
pub mod worker_registrations;

#[cfg(test)]
pub(crate) mod test_ctx;

pub use self::chunked::{IN_CHUNK_SIZE, fetch_in_chunks, for_each_chunk};
pub use self::context::{DbContext, DbMemory, HeldEvaluations, Hold, ProbeRequests, Wakeups};
pub use self::error::MissingRow;
pub use self::pool::{CacheDb, WebDb, WorkerDb};
