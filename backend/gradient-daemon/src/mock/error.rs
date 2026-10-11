/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MockError {
    #[error("unknown node {id}")]
    UnknownNode { id: String },
}
