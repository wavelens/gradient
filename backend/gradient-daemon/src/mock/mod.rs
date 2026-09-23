/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod artefact;
pub mod build;
pub mod ca_path;
pub mod cache_export;
pub mod conn;
pub mod control;
pub mod ingest;
pub mod nar;
pub mod spec;
pub mod store;
pub mod timing;

use crate::backend::{Backend, ConnInfo};
use crate::journal::Journal;
use harmonia_store_path::StorePath;
use spec::{DaemonConfig, Outcome};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use store::{MockStore, Origin};
use tokio::sync::Notify;

pub struct MockState {
    pub config: DaemonConfig,
    pub store: MockStore,
    pub journal: Arc<Journal>,
    pub overrides: Mutex<HashMap<String, Outcome>>,
    pub hangs: Mutex<HashMap<String, Arc<Notify>>>,
    pub attempts: Mutex<HashMap<String, u32>>,
    pub running: Mutex<BTreeMap<String, u32>>,
}

pub struct MockBackend(pub Arc<MockState>);

pub fn store_path(full: &str) -> anyhow::Result<StorePath> {
    Ok(StorePath::from_base_path(
        full.trim_start_matches("/nix/store/"),
    )?)
}

pub async fn seed_node(state: &MockState, id: &str) -> anyhow::Result<()> {
    let node = state
        .config
        .derivations
        .get(id)
        .ok_or_else(|| anyhow::anyhow!("unknown node {id}"))?;
    for (name, out) in &node.outputs {
        let nar = artefact::render(id, node, name)?;
        let refs = out
            .references
            .iter()
            .map(|r| store_path(r))
            .collect::<anyhow::Result<BTreeSet<_>>>()?;
        let info = MockStore::describe(&nar, refs, None, None);
        state
            .store
            .register(&store_path(&out.path)?, &nar, info, Origin::Seeded)
            .await?;
    }
    Ok(())
}

impl MockBackend {
    pub async fn new(
        mut config: DaemonConfig,
        root: PathBuf,
        base_db: Option<&Path>,
    ) -> anyhow::Result<Arc<Self>> {
        let seed = std::env::var("GRADIENT_DAEMON_SEED")
            .ok()
            .and_then(|s| s.parse().ok());
        if let Some(seed) = seed {
            config.timing.seed = seed;
            config
                .derivations
                .values_mut()
                .for_each(|n| n.timing.seed = seed);
        }
        tracing::info!(seed = config.timing.seed, "mock timing seed");

        let journal = Arc::new(Journal::new());
        let store = MockStore::open(root, base_db, journal.clone())?;
        let state = Arc::new(MockState {
            config,
            store,
            journal,
            overrides: Mutex::default(),
            hangs: Mutex::default(),
            attempts: Mutex::default(),
            running: Mutex::default(),
        });
        seed_present(&state).await?;
        Ok(Arc::new(Self(state)))
    }
}

fn seedable(state: &MockState, id: &str) -> bool {
    let node = &state.config.derivations[id];
    node.outputs.values().flat_map(|o| &o.references).all(|r| {
        node.outputs.values().any(|o| &o.path == r)
            || store_path(r)
                .and_then(|p| state.store.is_valid(&p))
                .unwrap_or(false)
    })
}

async fn seed_present(state: &MockState) -> anyhow::Result<()> {
    let worker = &state.config.worker;
    let mut pending: Vec<&String> = state
        .config
        .derivations
        .iter()
        .filter(|(_, n)| n.present.workers.contains(worker))
        .map(|(id, _)| id)
        .collect();
    while !pending.is_empty() {
        let before = pending.len();
        let mut blocked = Vec::new();
        for id in pending {
            if seedable(state, id) {
                seed_node(state, id).await?;
            } else {
                blocked.push(id);
            }
        }

        anyhow::ensure!(
            blocked.len() < before,
            "present.workers is not closed under references: {blocked:?}"
        );
        pending = blocked;
    }
    Ok(())
}

impl Backend for MockBackend {
    type Handler = conn::MockConn;

    fn journal(&self) -> &Journal {
        &self.0.journal
    }

    fn handler(self: &Arc<Self>, conn: ConnInfo) -> conn::MockConn {
        conn::MockConn {
            state: self.0.clone(),
            conn,
        }
    }

    fn control(
        &self,
        cmd: &str,
        args: &serde_json::Value,
    ) -> Option<anyhow::Result<serde_json::Value>> {
        control::dispatch(&self.0, cmd, args)
    }
}
