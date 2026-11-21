/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};

use chrono::NaiveDateTime;
use gradient_db::metrics::projects_trusting_worker_credentials_with_metrics_since::WorkerCredentialKey;
use gradient_pool::estimate::{GroupKey, Sample, fewer_cores, input_overlap};
use gradient_pool::score::weights::{MIN_INPUT_OVERLAP, OWN_SAMPLE_WINDOW};
use gradient_types::ids::{DerivationId, ProjectId, TeamWorkerId, WorkerRegistrationId};
use uuid::Uuid;

pub(crate) struct LoadedSample {
    pub id: Uuid,
    pub created_at: NaiveDateTime,
    pub oom_killed: bool,
    pub worker_registration_id: Option<WorkerRegistrationId>,
    pub reporting_team_worker_id: Option<TeamWorkerId>,
    pub project: Option<ProjectId>,
    pub sample: Sample,
}

#[derive(Default)]
pub struct Changes {
    everything: bool,
    pnames: HashSet<(String, String)>,
    derivations: HashSet<DerivationId>,
}

impl Changes {
    pub fn everything() -> Self {
        Self {
            everything: true,
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.everything && self.pnames.is_empty() && self.derivations.is_empty()
    }

    pub fn touches(&self, key: &GroupKey, derivation: DerivationId) -> bool {
        self.everything
            || self.derivations.contains(&derivation)
            || self
                .pnames
                .contains(&(key.architecture.clone(), key.pname.clone()))
    }

    fn record(&mut self, sample: &Sample) {
        self.pnames
            .insert((sample.architecture.clone(), sample.pname.clone()));
        self.derivations.insert(sample.derivation);
    }
}

#[derive(Default)]
pub(crate) struct SampleStore {
    seen: HashSet<Uuid>,
    rows: Vec<LoadedSample>,
    by_pname: HashMap<(String, String), Vec<usize>>,
    out_of_memory_sample_row_positions_by_derivation: HashMap<DerivationId, Vec<usize>>,
    pub newest: Option<NaiveDateTime>,
    pub samples_changed_since_last_fit: bool,
    pub changes: Changes,
    pub trusted_projects_by_worker_credential: HashMap<WorkerCredentialKey, HashSet<ProjectId>>,
}

impl SampleStore {
    pub fn add(&mut self, loaded: Vec<LoadedSample>) {
        for row in loaded {
            if !self.seen.insert(row.id) {
                continue;
            }
            self.changes.record(&row.sample);
            self.samples_changed_since_last_fit = true;
            self.newest = self.newest.max(Some(row.created_at));
            if row.oom_killed && row.sample.cores.is_some() {
                self.out_of_memory_sample_row_positions_by_derivation
                    .entry(row.sample.derivation)
                    .or_default()
                    .push(self.rows.len());
            }
            self.by_pname
                .entry((row.sample.architecture.clone(), row.sample.pname.clone()))
                .or_default()
                .push(self.rows.len());
            self.rows.push(row);
        }
    }

    pub fn samples(&self) -> Vec<Sample> {
        self.rows.iter().map(|r| r.sample.clone()).collect()
    }

    fn rows_with_worker_credential_trusted_by_project<'a>(
        &'a self,
        positions: &'a [usize],
        project: ProjectId,
    ) -> impl DoubleEndedIterator<Item = &'a LoadedSample> {
        positions
            .iter()
            .map(|&i| &self.rows[i])
            .filter(move |row| self.project_trusts_sample_worker_credentials(row, project))
    }

    pub fn own(&self, key: &GroupKey, project: ProjectId) -> Vec<(f64, &Sample)> {
        let Some(rows) = self
            .by_pname
            .get(&(key.architecture.clone(), key.pname.clone()))
        else {
            return Vec::new();
        };
        self.rows_with_worker_credential_trusted_by_project(rows, project)
            .rev()
            .map(|row| &row.sample)
            .map(|sample| {
                (
                    input_overlap(&key.input_pnames, &sample.input_pnames),
                    sample,
                )
            })
            .filter(|(overlap, _)| *overlap >= MIN_INPUT_OVERLAP)
            .take(OWN_SAMPLE_WINDOW)
            .collect()
    }

    pub fn project_trusts_sample_worker_credentials(
        &self,
        sample: &LoadedSample,
        project: ProjectId,
    ) -> bool {
        let credentials = [
            sample
                .worker_registration_id
                .map(WorkerCredentialKey::ProjectRegisteredWorker),
            sample
                .reporting_team_worker_id
                .map(WorkerCredentialKey::TeamWorkerId),
        ];
        credentials.into_iter().flatten().any(|credential| {
            self.trusted_projects_by_worker_credential
                .get(&credential)
                .is_some_and(|projects| projects.contains(&project))
        })
    }

    pub fn max_cores(&self, derivation: DerivationId, project: ProjectId) -> Option<u32> {
        let positions = self
            .out_of_memory_sample_row_positions_by_derivation
            .get(&derivation)?;
        positions
            .iter()
            .map(|&i| &self.rows[i])
            .filter(|row| {
                row.project == Some(project)
                    || self.project_trusts_sample_worker_credentials(row, project)
            })
            .filter_map(|row| row.sample.cores)
            .min()
            .map(fewer_cores)
    }

    pub fn prune_before(&mut self, oldest: NaiveDateTime) {
        if self.rows.iter().all(|r| r.created_at >= oldest) {
            return;
        }
        let newest = self.newest;
        let kept: Vec<LoadedSample> = std::mem::take(&mut self.rows)
            .into_iter()
            .filter(|r| r.created_at >= oldest)
            .collect();
        *self = Self {
            trusted_projects_by_worker_credential: std::mem::take(
                &mut self.trusted_projects_by_worker_credential,
            ),
            ..Self::default()
        };
        self.add(kept);
        self.newest = newest;
        self.samples_changed_since_last_fit = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::ids::project_id;

    fn trusted_worker_registration_id() -> WorkerRegistrationId {
        WorkerRegistrationId::new(Uuid::from_u128(1))
    }

    fn empty_sample_store_trusting_fixed_test_worker_for_fixture_project() -> SampleStore {
        SampleStore {
            trusted_projects_by_worker_credential: HashMap::from([(
                WorkerCredentialKey::ProjectRegisteredWorker(trusted_worker_registration_id()),
                HashSet::from([project_id()]),
            )]),
            ..SampleStore::default()
        }
    }

    fn row(id: Uuid, derivation: DerivationId, cores: u32, oom: bool) -> LoadedSample {
        LoadedSample {
            id,
            created_at: gradient_types::now(),
            oom_killed: oom,
            worker_registration_id: Some(trusted_worker_registration_id()),
            reporting_team_worker_id: None,
            project: None,
            sample: Sample {
                derivation,
                architecture: "x86_64-linux".into(),
                pname: "torch".into(),
                input_pnames: vec!["cmake".into()],
                cores: Some(cores),
                peak_ram_mb: Some(100_000),
                build_ms: (!oom).then_some(1.0),
                output_nar_bytes: None,
                lower_bound: oom,
            },
        }
    }

    #[test]
    fn a_row_read_twice_counts_once() {
        let mut store = SampleStore::default();
        let id = Uuid::now_v7();
        let derivation = DerivationId::now_v7();
        store.add(vec![row(id, derivation, 64, false)]);
        store.add(vec![row(id, derivation, 64, false)]);
        assert_eq!(store.samples().len(), 1);
    }

    #[test]
    fn the_core_cap_follows_the_fewest_cores_a_derivation_ran_out_of_memory_with() {
        let mut store = empty_sample_store_trusting_fixed_test_worker_for_fixture_project();
        let derivation = DerivationId::now_v7();
        store.add(vec![
            row(Uuid::now_v7(), derivation, 64, true),
            row(Uuid::now_v7(), derivation, 43, true),
        ]);
        assert_eq!(store.max_cores(derivation, project_id()), Some(29));
        assert_eq!(store.max_cores(DerivationId::now_v7(), project_id()), None);
    }

    #[test]
    fn a_row_without_a_build_time_that_was_not_killed_caps_nothing() {
        let mut store = empty_sample_store_trusting_fixed_test_worker_for_fixture_project();
        let derivation = DerivationId::now_v7();
        let mut lost = row(Uuid::now_v7(), derivation, 64, false);
        lost.sample.build_ms = None;
        lost.sample.lower_bound = true;
        store.add(vec![lost]);

        assert_eq!(store.max_cores(derivation, project_id()), None);
    }

    #[test]
    fn test_untrusted_worker_out_of_memory_row_gives_no_own_sample_but_caps_cores_only_for_project()
    {
        let mut store = empty_sample_store_trusting_fixed_test_worker_for_fixture_project();
        let derivation = DerivationId::now_v7();
        let mut untrusted = row(Uuid::now_v7(), derivation, 16, true);
        untrusted.worker_registration_id = None;
        untrusted.project = Some(project_id());
        store.add(vec![untrusted]);

        assert!(store.own(&key(&["cmake"]), project_id()).is_empty());
        assert_eq!(store.max_cores(derivation, project_id()), Some(11));
        assert_eq!(store.max_cores(derivation, ProjectId::now_v7()), None);
        assert_eq!(store.max_cores(DerivationId::now_v7(), project_id()), None);
    }

    fn with_inputs(inputs: &[&str]) -> LoadedSample {
        let mut sample = row(Uuid::now_v7(), DerivationId::now_v7(), 16, false);
        sample.sample.input_pnames = inputs.iter().map(|i| (*i).to_owned()).collect();
        sample
    }

    fn key(inputs: &[&str]) -> GroupKey {
        with_inputs(inputs).sample.group()
    }

    #[test]
    fn a_build_with_other_inputs_under_the_same_pname_is_not_an_own_build() {
        let mut store = empty_sample_store_trusting_fixed_test_worker_for_fixture_project();
        store.add(vec![with_inputs(&["cmake", "cudatoolkit"])]);

        assert!(
            store
                .own(&key(&["autoPatchelfHook", "python3-wheel"]), project_id())
                .is_empty()
        );
    }

    #[test]
    fn an_own_build_counts_by_the_share_of_inputs_both_have() {
        let mut store = empty_sample_store_trusting_fixed_test_worker_for_fixture_project();
        store.add(vec![with_inputs(&["a", "b", "c", "d"])]);

        let own = store.own(&key(&["a", "b", "c", "e"]), project_id());
        assert_eq!(own.len(), 1);
        assert!((own[0].0 - 0.6).abs() < 1e-9, "{}", own[0].0);
    }

    #[test]
    fn newer_builds_of_another_variant_do_not_push_out_the_matching_history() {
        let mut store = empty_sample_store_trusting_fixed_test_worker_for_fixture_project();
        store.add(vec![with_inputs(&["cmake", "cudatoolkit"])]);
        store.add(
            (0..OWN_SAMPLE_WINDOW)
                .map(|_| with_inputs(&["autoPatchelfHook", "python3-wheel"]))
                .collect(),
        );

        assert_eq!(
            store
                .own(&key(&["cmake", "cudatoolkit"]), project_id())
                .len(),
            1
        );
    }

    fn build_sample_row_with_worker_registration_and_out_of_memory_core_count(
        worker_registration_id: Option<WorkerRegistrationId>,
        derivation: DerivationId,
        cores: u32,
    ) -> LoadedSample {
        LoadedSample {
            worker_registration_id,
            ..row(Uuid::now_v7(), derivation, cores, true)
        }
    }

    #[test]
    fn project_build_samples_and_out_of_memory_core_cap_include_trusted_credential_samples_exclude_untrusted_credential_samples_and_pooled_architecture_fit_includes_samples_without_credentials()
     {
        let worker_registration_id_trusted_only_by_other_project =
            WorkerRegistrationId::new(Uuid::from_u128(2));
        let mut store = empty_sample_store_trusting_fixed_test_worker_for_fixture_project();
        store.trusted_projects_by_worker_credential.insert(
            WorkerCredentialKey::ProjectRegisteredWorker(
                worker_registration_id_trusted_only_by_other_project,
            ),
            HashSet::from([ProjectId::new(Uuid::from_u128(3))]),
        );
        let derivation = DerivationId::now_v7();
        store.add(vec![
            build_sample_row_with_worker_registration_and_out_of_memory_core_count(
                Some(trusted_worker_registration_id()),
                derivation,
                64,
            ),
            build_sample_row_with_worker_registration_and_out_of_memory_core_count(
                Some(worker_registration_id_trusted_only_by_other_project),
                derivation,
                8,
            ),
            build_sample_row_with_worker_registration_and_out_of_memory_core_count(
                None, derivation, 4,
            ),
        ]);

        let own_build_sample_counts: Vec<Option<u32>> = store
            .own(&key(&["cmake"]), project_id())
            .into_iter()
            .map(|(_, sample)| sample.cores)
            .collect();
        assert_eq!(
            (
                own_build_sample_counts,
                store.max_cores(derivation, project_id())
            ),
            (vec![Some(64)], Some(43)),
            "Only trusted workers supply the project with its own samples and core cap."
        );
        assert_eq!(
            store.samples().len(),
            3,
            "Samples without worker credentials still go into the pooled fit for each architecture."
        );
    }

    #[tokio::test]
    async fn build_history_refresh_carries_derivation_metric_worker_registration_team_worker_project_and_trust_into_sample()
     {
        use gradient_entity::{derivation, derivation_metric, project, user, worker_registration};
        use gradient_fixtures::ids::{project_id, user_id};
        use gradient_fixtures::{rows, test_date};

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let conn = db.connection();
        gradient_db::rows::insert::<user::Entity, _>(&conn, rows::user())
            .await
            .expect("the owner is written");
        gradient_db::rows::insert::<project::Entity, _>(&conn, rows::project())
            .await
            .expect("the project is written");
        let main_project_worker_registration =
            gradient_db::rows::insert::<worker_registration::Entity, _>(
                &conn,
                worker_registration::Model {
                    id: WorkerRegistrationId::now_v7(),
                    peer_id: project_id(),
                    worker_id: "main-worker".into(),
                    created_by: Some(user_id()),
                    ..rows::worker_registration(true, false)
                },
            )
            .await
            .expect("the registration is written");
        let derivation_id = DerivationId::now_v7();
        gradient_db::rows::insert::<derivation::Entity, _>(
            &conn,
            rows::derivation_named(derivation_id, "sample"),
        )
        .await
        .expect("the derivation is written");
        gradient_db::rows::insert::<derivation_metric::Entity, _>(
            &conn,
            derivation_metric::Model {
                id: gradient_entity::ids::DerivationMetricId::now_v7(),
                derivation: derivation_id,
                architecture: "x86_64-linux".into(),
                peak_ram_mb: Some(512),
                worker_id: "main-worker".into(),
                worker_registration: Some(main_project_worker_registration.id),
                project: Some(project_id()),
                created_at: test_date(),
                ..Default::default()
            },
        )
        .await
        .expect("the metric row is written");

        let history = super::super::BuildHistory::default();
        history.refresh(&conn).await.expect("the history is loaded");

        {
            let store = history.store();
            assert_eq!(
                store
                    .rows
                    .iter()
                    .map(|row| (
                        row.worker_registration_id,
                        row.reporting_team_worker_id,
                        row.project
                    ))
                    .collect::<Vec<_>>(),
                vec![(
                    Some(main_project_worker_registration.id),
                    None,
                    Some(project_id())
                )],
                "Refresh operations copy the worker registration and project of stored metric rows into the loaded samples."
            );
            assert!(
                store.project_trusts_sample_worker_credentials(&store.rows[0], project_id()),
                "Projects that registered the reporting worker trust the sample after the refresh."
            );
        }
        db.finish().await;
    }
}
