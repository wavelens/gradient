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
use gradient_db::metrics::projects_trusting_worker_credentials_with_metrics_since::load_project_ids_trusting_worker_credentials_from_derivation_metrics_since;
use gradient_pool::estimate::{BuildEstimate, GroupKey, Model, Sample};
use gradient_types::ids::{DerivationId, ProjectId};
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
        let oldest = self.oldest_retained_sample_creation_time();
        let loaded = load::load_since(db, newest, oldest).await?;
        let trusted_projects_by_worker_credential =
            load_project_ids_trusting_worker_credentials_from_derivation_metrics_since(
                db,
                load::read_since(None, oldest),
            )
            .await?;
        {
            let mut store = self.store();
            store.add(loaded);
            store.trusted_projects_by_worker_credential = trusted_projects_by_worker_credential;
        }
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
        let trusted_out_of_memory_worker_registration_id =
            gradient_types::ids::WorkerRegistrationId::new(uuid::Uuid::from_u128(1));
        history.store().trusted_projects_by_worker_credential = HashMap::from([(
            gradient_db::metrics::projects_trusting_worker_credentials_with_metrics_since::WorkerCredentialKey::ProjectRegisteredWorker(
                trusted_out_of_memory_worker_registration_id,
            ),
            std::collections::HashSet::from([gradient_fixtures::ids::project_id()]),
        )]);
        history.store().add(vec![store::LoadedSample {
            id: uuid::Uuid::now_v7(),
            created_at: gradient_types::now(),
            oom_killed: true,
            worker_registration_id: Some(trusted_out_of_memory_worker_registration_id),
            reporting_team_worker_id: None,
            project: None,
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

    pub fn estimate(
        &self,
        key: &GroupKey,
        derivation: DerivationId,
        project: ProjectId,
    ) -> BuildEstimate {
        let store = self.store();
        let mut estimate = self.model.load().estimate(key, &store.own(key, project));
        estimate.max_cores = store.max_cores(derivation, project);
        estimate
    }

    pub fn take_changes(&self) -> Changes {
        std::mem::take(&mut self.store().changes)
    }

    pub fn estimate_all<K>(
        &self,
        wanted: Vec<(K, GroupKey, DerivationId, ProjectId)>,
    ) -> Vec<(K, BuildEstimate)> {
        let mut own: HashMap<(GroupKey, ProjectId), Vec<(f64, Sample)>> = HashMap::new();
        let wanted: Vec<(K, (GroupKey, ProjectId), Option<u32>)> = {
            let store = self.store();
            wanted
                .into_iter()
                .map(|(id, key, derivation, project)| {
                    let max_cores = store.max_cores(derivation, project);
                    let scope = (key, project);
                    if !own.contains_key(&scope) {
                        let samples = store
                            .own(&scope.0, project)
                            .into_iter()
                            .map(|(overlap, sample)| (overlap, sample.clone()))
                            .collect();
                        own.insert(scope.clone(), samples);
                    }

                    (id, scope, max_cores)
                })
                .collect()
        };

        let model = self.model.load();
        let by_scope: HashMap<&(GroupKey, ProjectId), BuildEstimate> = own
            .iter()
            .map(|(scope, samples)| {
                let samples: Vec<(f64, &Sample)> = samples.iter().map(|(o, s)| (*o, s)).collect();
                (scope, model.estimate(&scope.0, &samples))
            })
            .collect();

        wanted
            .into_iter()
            .map(|(id, scope, max_cores)| {
                let estimate = BuildEstimate {
                    max_cores,
                    ..by_scope[&scope].clone()
                };
                (id, estimate)
            })
            .collect()
    }
}
