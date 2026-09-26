/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Protocol-level concerns: credentials, job updates, NAR transfer, scoring.

pub(crate) mod compression;
pub mod credentials;
pub mod eval_cache_recv;
pub mod job;
pub(crate) mod nar_daemon_import;
pub(crate) mod prefetch;
pub(crate) mod progress;
pub mod scorer;
