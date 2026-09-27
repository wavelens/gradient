/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Server-wide upload admission: nothing is uploaded before it is granted.

mod core;
mod shell;

pub use self::core::{AdmissionCore, Decision, Limits, ObjectKey, Outcome, Request, SessionId};
pub use self::shell::{AdmissionSession, Admitted, UploadAdmission, UploadPermit};
