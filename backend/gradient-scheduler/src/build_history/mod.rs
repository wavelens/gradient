/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod load;
mod store;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use arc_swap::ArcSwap;
use gradient_pool::estimate::{BuildEstimate, GroupKey, Model};
use gradient_types::ids::DerivationId;
use sea_orm::{ConnectionTrait, DbErr};

use store::SampleStore;

#[derive(Default)]
pub struct BuildHistory {
    store: Mutex<SampleStore>,
    model: ArcSwap<Model>,
    fitted: AtomicBool,
}

impl BuildHistory {
    fn store(&self) -> MutexGuard<'_, SampleStore> {
        self.store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub async fn refresh(&self, db: &impl ConnectionTrait) -> Result<(), DbErr> {
        let newest = self.store().newest;
        let loaded = load::load_since(db, newest).await?;
        self.store().add(loaded);
        if !self.fitted.load(Ordering::Acquire) {
            self.refit(0);
        }
        Ok(())
    }

    pub fn refit(&self, retention_days: u32) {
        let samples = {
            let mut store = self.store();
            if retention_days > 0 {
                store.prune_before(
                    gradient_types::now() - chrono::Duration::days(i64::from(retention_days)),
                );
            }
            store.samples()
        };
        self.model.store(Arc::new(Model::fit(&samples)));
        self.fitted.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub fn with_out_of_memory_kill(
        key: GroupKey,
        derivation: DerivationId,
        cores: u32,
        peak_ram_mb: u64,
    ) -> Self {
        let history = Self::default();
        history.store().add(vec![store::LoadedSample {
            id: uuid::Uuid::now_v7(),
            created_at: gradient_types::now(),
            oom_killed: true,
            sample: gradient_pool::estimate::Sample {
                derivation,
                architecture: key.architecture,
                pname: key.pname,
                input_pnames: key.input_pnames,
                cores: Some(cores),
                peak_ram_mb: Some(peak_ram_mb),
                build_ms: None,
                output_nar_bytes: None,
                lower_bound: true,
            },
        }]);
        history
    }

    pub fn estimate(&self, key: &GroupKey, derivation: DerivationId) -> BuildEstimate {
        let store = self.store();
        let mut estimate = self.model.load().estimate(key, &store.own(key));
        estimate.max_cores = store.max_cores(derivation);
        estimate
    }
}
