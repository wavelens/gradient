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
    Dead,
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
        match self.by_key.get(key).map(|of| (of, of.cluster.status)) {
            None | Some((_, ClusterJobStatus::Completed)) => Route::Single,
            Some((of, ClusterJobStatus::Queued)) => Route::Member(of.clone()),
            Some((_, ClusterJobStatus::Running)) => Route::Held,
            Some((_, ClusterJobStatus::Failed | ClusterJobStatus::Aborted)) => Route::Dead,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::ids::ClusterJobId;

    fn membership(status: ClusterJobStatus) -> Membership {
        let mut of = crate::cluster::book::book_tests::member_of(ClusterJobId::now_v7(), 1);
        of.cluster.status = status;
        Membership {
            by_key: HashMap::from([("build:a".to_owned(), of)]),
        }
    }

    #[test]
    fn a_member_routes_by_its_clusters_status() {
        let route = |status| membership(status).route("build:a");

        assert!(matches!(route(ClusterJobStatus::Queued), Route::Member(_)));
        assert!(matches!(route(ClusterJobStatus::Running), Route::Held));
        assert!(matches!(route(ClusterJobStatus::Completed), Route::Single));
        assert!(matches!(route(ClusterJobStatus::Failed), Route::Dead));
        assert!(matches!(route(ClusterJobStatus::Aborted), Route::Dead));
        assert!(matches!(
            membership(ClusterJobStatus::Queued).route("eval:x"),
            Route::Single
        ));
    }
}
