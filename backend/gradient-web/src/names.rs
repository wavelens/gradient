/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{ErrorCode, WebError};
pub use gradient_api_types::index_and_display_names::*;

impl From<NameError> for WebError {
    fn from(err: NameError) -> Self {
        match err {
            NameError::Index { subject } => {
                Self::BadRequest(ErrorCode::INVALID_NAME, format!("Invalid {subject}"))
            }
            NameError::Display { label, reason } => {
                Self::BadRequest(ErrorCode::BAD_REQUEST, format!("Invalid {label}: {reason}"))
            }
        }
    }
}
