/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod core;
mod shell;

pub use self::core::{AdmissionCore, Limits, ObjectKey, Request, SessionId};
pub use self::shell::{
    AdmissionSession, AdmissionStats, Admitted, HeldPermit, UploadAdmission, UploadPermit,
};
