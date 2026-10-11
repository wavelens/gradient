/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::refusal::{Refusal, RefusalStatus};

#[derive(Debug, thiserror::Error)]
pub(crate) enum EndpointRefusal {
    #[error("Not Found")]
    UnknownRoute,
}

impl Refusal for EndpointRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::UnknownRoute => RefusalStatus::NotFound,
        }
    }
}
