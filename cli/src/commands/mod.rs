/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod attr_spec;
pub mod base;
pub mod build;
#[cfg(feature = "nix")]
pub mod build_nix;
pub mod builds;
pub mod builds_log;
pub mod cache;
pub mod cache_nar;
pub mod cache_upload;
#[cfg(feature = "nix")]
pub mod cache_upload_nix;
pub mod completion;
pub mod download;
#[cfg(feature = "eval")]
pub mod eval;
pub mod evaluation;
pub mod generate;
pub mod live_events;
pub mod logged_in_user_account_group_commands;
pub mod logs;
pub mod logstream;
pub mod mcp;
pub mod pr;
pub mod project;
pub mod run;
mod run_forward;
mod run_logs;
mod run_terminal;
mod run_waiting;
pub mod saved_account_commands;
pub mod search_command;
pub mod task;
pub mod task_subcommands;
pub mod watch;
pub mod worker;
