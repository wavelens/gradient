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
pub mod nar;
pub(crate) mod nar_daemon_import;
pub mod nar_recv;
pub(crate) mod prefetch;
pub mod scorer;
pub(crate) mod substitute_relay;
