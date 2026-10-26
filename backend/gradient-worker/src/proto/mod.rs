/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub(crate) mod compression;
pub mod credentials;
pub mod eval_cache_recv;
pub mod eval_shard_replies;
pub mod job;
#[cfg_attr(not(test), expect(dead_code, reason = "job tasks adopt it next"))]
pub mod job_log;
pub(crate) mod nar_daemon_import;
pub(crate) mod prefetch;
pub(crate) mod progress;
pub mod scorer;
