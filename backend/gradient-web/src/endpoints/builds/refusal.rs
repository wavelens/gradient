/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::refusal::{Refusal, RefusalStatus};

#[derive(Debug, thiserror::Error)]
pub(crate) enum BuildReadRefusal {
    #[error("Invalid download token")]
    InvalidDownloadToken,
    #[error("range must look like L120-L130")]
    MalformedRange,
    #[error("invalid range start")]
    InvalidRangeStart,
    #[error("invalid range end")]
    InvalidRangeEnd,
}

impl Refusal for BuildReadRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::InvalidDownloadToken => RefusalStatus::Unauthorized,
            Self::MalformedRange | Self::InvalidRangeStart | Self::InvalidRangeEnd => {
                RefusalStatus::BadRequest
            }
        }
    }
}
