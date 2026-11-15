/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeSet, HashMap};

use gradient_wire::types::JobKind;

use crate::jobs::PendingJob;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PendingJobSortKey(bool, chrono::NaiveDateTime, String);

impl PendingJobSortKey {
    fn new(job_id: &str, job: &PendingJob) -> Self {
        let (not_prioritized, ready_at) = sort_position(job);
        Self(not_prioritized, ready_at, job_id.to_owned())
    }
}

fn sort_position(job: &PendingJob) -> (bool, chrono::NaiveDateTime) {
    (!job.prioritized(), job.ready_at())
}

fn replace_pending_job_sort_key_if_sort_position_changed(
    dispatch_order_sort_key_sets_by_job_kind: &mut [BTreeSet<PendingJobSortKey>; 3],
    job_id: &str,
    job: &PendingJob,
    before: (bool, chrono::NaiveDateTime),
) {
    let after = sort_position(job);
    if before == after {
        return;
    }
    let set =
        &mut dispatch_order_sort_key_sets_by_job_kind[job_kind_to_array_index(&job.job_kind())];
    set.remove(&PendingJobSortKey(before.0, before.1, job_id.to_owned()));
    set.insert(PendingJobSortKey(after.0, after.1, job_id.to_owned()));
}

fn job_kind_to_array_index(kind: &JobKind) -> usize {
    match kind {
        JobKind::Flake => 0,
        JobKind::Build => 1,
        JobKind::Exec => 2,
    }
}

#[derive(Debug, Default)]
pub struct PendingJobs {
    jobs_by_id: HashMap<String, PendingJob>,
    dispatch_order_sort_key_sets_by_job_kind: [BTreeSet<PendingJobSortKey>; 3],
}

impl PendingJobs {
    pub fn insert(&mut self, job_id: String, job: PendingJob) {
        self.remove(&job_id);
        let kind = job_kind_to_array_index(&job.job_kind());
        self.dispatch_order_sort_key_sets_by_job_kind[kind]
            .insert(PendingJobSortKey::new(&job_id, &job));
        self.jobs_by_id.insert(job_id, job);
    }

    pub fn remove(&mut self, job_id: &str) -> Option<PendingJob> {
        let job = self.jobs_by_id.remove(job_id)?;
        self.remove_job_sort_key_from_kind_ordered_set(job_id, &job);
        Some(job)
    }

    fn remove_job_sort_key_from_kind_ordered_set(&mut self, job_id: &str, job: &PendingJob) {
        let kind = job_kind_to_array_index(&job.job_kind());
        self.dispatch_order_sort_key_sets_by_job_kind[kind]
            .remove(&PendingJobSortKey::new(job_id, job));
    }

    pub fn get(&self, job_id: &str) -> Option<&PendingJob> {
        self.jobs_by_id.get(job_id)
    }

    pub fn contains_key(&self, job_id: &str) -> bool {
        self.jobs_by_id.contains_key(job_id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &PendingJob)> {
        self.jobs_by_id.iter()
    }

    pub fn values(&self) -> impl Iterator<Item = &PendingJob> {
        self.jobs_by_id.values()
    }

    #[cfg(test)]
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.jobs_by_id.keys()
    }

    pub fn len(&self) -> usize {
        self.jobs_by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs_by_id.is_empty()
    }

    pub fn iter_pending_jobs_for_kind_in_dispatch_order(
        &self,
        kind: &JobKind,
    ) -> impl Iterator<Item = (&String, &PendingJob)> {
        self.dispatch_order_sort_key_sets_by_job_kind[job_kind_to_array_index(kind)]
            .iter()
            .filter_map(|PendingJobSortKey(_, _, job_id)| self.jobs_by_id.get_key_value(job_id))
    }

    pub fn update_all_pending_jobs_and_resort_jobs_with_changed_sort_key(
        &mut self,
        mut update: impl FnMut(&mut PendingJob),
    ) {
        let Self {
            jobs_by_id,
            dispatch_order_sort_key_sets_by_job_kind,
        } = self;
        for (job_id, job) in jobs_by_id.iter_mut() {
            let before = sort_position(job);
            update(job);
            replace_pending_job_sort_key_if_sort_position_changed(
                dispatch_order_sort_key_sets_by_job_kind,
                job_id,
                job,
                before,
            );
        }
    }

    pub fn update_pending_job_by_id_and_resort_if_sort_key_changed(
        &mut self,
        job_id: &str,
        update: impl FnOnce(&mut PendingJob),
    ) {
        let Some(job) = self.jobs_by_id.get_mut(job_id) else {
            return;
        };
        let before = sort_position(job);
        update(job);
        replace_pending_job_sort_key_if_sort_position_changed(
            &mut self.dispatch_order_sort_key_sets_by_job_kind,
            job_id,
            job,
            before,
        );
    }
}
