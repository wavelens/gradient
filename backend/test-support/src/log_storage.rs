/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::Mutex;

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

/// Recording log storage: keeps every `(build_id, text)` append in memory so
/// tests can assert what would have been written to the log.
///
/// `read` returns the concatenation of every append for that build (matching
/// the on-disk semantics of [`gradient_core::storage::FileLogStorage`]).
#[derive(Debug, Default)]
pub struct RecordingLogStorage {
    entries: Mutex<Vec<(BuildId, String)>>,
}

impl RecordingLogStorage {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a clone of every recorded `(build_id, text)` append.
    pub fn entries(&self) -> Vec<(BuildId, String)> {
        self.entries.lock().expect("recording log mutex").clone()
    }
}

impl LogStorage for RecordingLogStorage {
    fn append<'a>(&'a self, build_id: BuildId, text: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.entries
                .lock()
                .expect("recording log mutex")
                .push((build_id, text.to_owned()));
            Ok(())
        })
    }

    fn read<'a>(&'a self, build_id: BuildId) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let entries = self.entries.lock().expect("recording log mutex");
            Ok(entries
                .iter()
                .filter(|(b, _)| *b == build_id)
                .map(|(_, t)| t.as_str())
                .collect::<String>())
        })
    }

    fn delete<'a>(&'a self, build_id: BuildId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.entries
                .lock()
                .expect("recording log mutex")
                .retain(|(b, _)| *b != build_id);
            Ok(())
        })
    }
}

/// Pre-seeded in-memory log storage for fixture tests that need `read` to
/// return deterministic content without going through `append`.
#[derive(Debug, Default)]
pub struct InMemoryLogStorage {
    store: Mutex<HashMap<BuildId, String>>,
}

impl InMemoryLogStorage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn seed(&self, build_id: BuildId, text: impl Into<String>) {
        self.store
            .lock()
            .expect("in-memory log mutex")
            .insert(build_id, text.into());
    }
}

impl LogStorage for InMemoryLogStorage {
    fn append<'a>(&'a self, build_id: BuildId, text: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.store
                .lock()
                .expect("in-memory log mutex")
                .entry(build_id)
                .or_default()
                .push_str(text);
            Ok(())
        })
    }

    fn read<'a>(&'a self, build_id: BuildId) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            Ok(self
                .store
                .lock()
                .expect("in-memory log mutex")
                .get(&build_id)
                .cloned()
                .unwrap_or_default())
        })
    }

    fn delete<'a>(&'a self, build_id: BuildId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.store
                .lock()
                .expect("in-memory log mutex")
                .remove(&build_id);
            Ok(())
        })
    }
}
