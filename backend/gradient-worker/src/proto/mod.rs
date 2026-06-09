/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Protocol-level concerns: credentials, job updates, NAR transfer, scoring.

pub mod credentials;
pub mod job;
pub mod nar;
pub mod nar_import;
pub mod nar_recv;
pub mod scorer;
