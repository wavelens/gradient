/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Where a ready job goes: single dispatch, its waiting cluster, or nowhere
//! while its cluster is past `Queued`.

use std::collections::HashMap;

use gradient_db::MemberOf;
use gradient_entity::cluster_job::ClusterJobStatus;
use gradient_types::ids::{DerivationBuildId, EvaluationId};
use sea_orm::{ConnectionTrait, DbErr};

use crate::jobs::{build_job_key, eval_job_key};

pub(crate) enum Route {
    Single,
    Member(MemberOf),
    Held,
}

pub(crate) struct Membership {
    by_key: HashMap<String, MemberOf>,
}

impl Membership {
    pub(crate) async fn load<C: ConnectionTrait>(
        db: &C,
        evaluations: &[EvaluationId],
        anchors: &[DerivationBuildId],
    ) -> Result<Self, DbErr> {
        let by_key = gradient_db::cluster_membership(db, evaluations, anchors)
            .await?
            .into_iter()
            .filter_map(|of| {
                let key = match (of.member.evaluation, of.member.derivation_build) {
                    (Some(e), _) => eval_job_key(e),
                    (None, Some(a)) => build_job_key(a),
                    (None, None) => return None,
                };
                Some((key, of))
            })
            .collect();
        Ok(Self { by_key })
    }

    pub(crate) fn route(&self, key: &str) -> Route {
        match self.by_key.get(key) {
            None => Route::Single,
            Some(of) if of.cluster.status == ClusterJobStatus::Queued => Route::Member(of.clone()),
            Some(_) => Route::Held,
        }
    }
}
