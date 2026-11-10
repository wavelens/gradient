/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod load;
mod store;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use arc_swap::ArcSwap;
use gradient_pool::estimate::{BuildEstimate, GroupKey, Model, Sample};
use gradient_types::ids::DerivationId;
use sea_orm::{ConnectionTrait, DbErr};
use tracing::warn;

pub use store::Changes;
use store::SampleStore;

#[derive(Default)]
pub struct BuildHistory {
    store: Mutex<SampleStore>,
    model: ArcSwap<Model>,
    fitted: AtomicBool,
    retention_days: u32,
}

impl BuildHistory {
    fn store(&self) -> MutexGuard<'_, SampleStore> {
        self.store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn new_with_retention_days(retention_days: u32) -> Self {
        Self {
            retention_days,
            ..Self::default()
        }
    }

    fn oldest_retained_sample_creation_time(&self) -> Option<chrono::NaiveDateTime> {
        (self.retention_days > 0)
            .then(|| gradient_types::now() - chrono::Duration::days(i64::from(self.retention_days)))
    }

    pub async fn refresh(&self, db: &impl ConnectionTrait) -> Result<(), DbErr> {
        let newest = self.store().newest;
        let loaded =
            load::load_since(db, newest, self.oldest_retained_sample_creation_time()).await?;
        self.store().add(loaded);
        if !self.fitted.load(Ordering::Acquire) {
            self.refit().await;
        }
        Ok(())
    }

    pub async fn refit(&self) {
        let samples = {
            let mut store = self.store();
            if let Some(oldest) = self.oldest_retained_sample_creation_time() {
                store.prune_before(oldest);
            }
            if self.fitted.load(Ordering::Acquire) && !store.samples_changed_since_last_fit {
                return;
            }
            store.samples_changed_since_last_fit = false;
            store.samples()
        };
        let model = match tokio::task::spawn_blocking(move || Model::fit(&samples)).await {
            Ok(model) => model,
            Err(e) => {
                warn!(error = %e, "fitting the build history model failed; keeping the last model");
                self.store().samples_changed_since_last_fit = true;
                return;
            }
        };
        self.model.store(Arc::new(model));
        self.fitted.store(true, Ordering::Release);
        self.store().changes = Changes::everything();
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

    pub fn take_changes(&self) -> Changes {
        std::mem::take(&mut self.store().changes)
    }

    pub fn estimate_all<K>(
        &self,
        wanted: Vec<(K, GroupKey, DerivationId)>,
    ) -> Vec<(K, BuildEstimate)> {
        let mut own: HashMap<GroupKey, Vec<(f64, Sample)>> = HashMap::new();
        let wanted: Vec<(K, GroupKey, Option<u32>)> = {
            let store = self.store();
            wanted
                .into_iter()
                .map(|(id, key, derivation)| {
                    if !own.contains_key(&key) {
                        let samples = store
                            .own(&key)
                            .into_iter()
                            .map(|(overlap, sample)| (overlap, sample.clone()))
                            .collect();
                        own.insert(key.clone(), samples);
                    }

                    (id, key, store.max_cores(derivation))
                })
                .collect()
        };

        let model = self.model.load();
        let by_key: HashMap<&GroupKey, BuildEstimate> = own
            .iter()
            .map(|(key, samples)| {
                let samples: Vec<(f64, &Sample)> = samples.iter().map(|(o, s)| (*o, s)).collect();
                (key, model.estimate(key, &samples))
            })
            .collect();

        wanted
            .into_iter()
            .map(|(id, key, max_cores)| {
                let estimate = BuildEstimate {
                    max_cores,
                    ..by_key[&key].clone()
                };
                (id, estimate)
            })
            .collect()
    }
}
