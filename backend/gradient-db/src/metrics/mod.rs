/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod build_times;
pub mod cache_traffic;
pub mod infra;
pub mod observations;
pub mod projects_trusting_worker_credentials_with_metrics_since;
pub mod resource_usage;
pub mod rollup;
pub mod series;
pub mod worker_load;
pub mod worker_samples;

#[cfg(test)]
mod add_worker_credential_and_project_columns_to_derivation_metric_migration_tests;
