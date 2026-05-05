/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use futures::future::BoxFuture;
use gradient_core::storage::LogStorage;
use gradient_core::types::ids::BuildId;

/// Minimal no-op log storage for tests.
#[derive(Debug, Default)]
pub struct NoopLogStorage;

impl LogStorage for NoopLogStorage {
    fn append<'a>(&'a self, _build_id: BuildId, _text: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn read<'a>(&'a self, _build_id: BuildId) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { Ok(String::new()) })
    }
    fn delete<'a>(&'a self, _build_id: BuildId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}
