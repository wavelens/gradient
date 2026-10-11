/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use arc_swap::ArcSwap;
use gradient_pool::score::{InstanceContext, ScoringPolicy};

use crate::assessment_memo::AssessmentMemo;
use crate::instance::EvalHistory;

#[derive(Clone)]
pub(crate) struct Scoring {
    pub(crate) policy: Arc<dyn ScoringPolicy>,
    pub(crate) instance: Arc<ArcSwap<InstanceContext>>,
    pub(crate) eval_history: Arc<ArcSwap<EvalHistory>>,
    pub(crate) assessments: Arc<std::sync::Mutex<AssessmentMemo>>,
}

impl Scoring {
    pub(crate) fn new(policy: Arc<dyn ScoringPolicy>) -> Self {
        Self {
            policy,
            instance: Arc::new(ArcSwap::from_pointee(InstanceContext::default())),
            eval_history: Arc::new(ArcSwap::from_pointee(EvalHistory::default())),
            assessments: Arc::default(),
        }
    }
}
