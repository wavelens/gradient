/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::build_request::Started;
use crate::session::Session;
use gradient_derivation::Derivation;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use gradient_util::log_lines::PrefixedLines;
use gradient_util::store_path::strip_nix_store_prefix;
use harmonia_protocol::daemon::wire::types2::FailureStatus;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

const POLL: Duration = Duration::from_secs(1);
const FAILURE_LINES: usize = 25;

pub type Outputs = BTreeMap<String, String>;

pub struct BuildFailure {
    pub status: FailureStatus,
    pub message: String,
}

pub type DrvResult = Result<Outputs, BuildFailure>;

pub struct BuildOutcome {
    pub results: Vec<(String, DrvResult)>,
}

struct Followed {
    offset: usize,
    lines: PrefixedLines,
}

pub fn failure_status(status: BuildStatus) -> FailureStatus {
    match status {
        BuildStatus::FailedTimeout => FailureStatus::TimedOut,
        BuildStatus::DependencyFailed => FailureStatus::DependencyFailed,
        BuildStatus::FailedPermanent => FailureStatus::PermanentFailure,
        BuildStatus::FailedTransient => FailureStatus::TransientFailure,
        _ => FailureStatus::MiscFailure,
    }
}

fn settles_the_request(status: BuildStatus) -> bool {
    status.is_terminal_success() || status.is_terminal_failure()
}

#[derive(Default)]
struct Waiting {
    systems: Option<String>,
}

impl Waiting {
    fn observe(&mut self, evaluation: &MEvaluation) -> Option<String> {
        if EvaluationStatus::TERMINAL.contains(&evaluation.status) {
            return None;
        }

        let systems = missing_systems(evaluation);
        let announce = systems
            .as_ref()
            .filter(|s| self.systems.as_ref() != Some(*s))
            .map(|s| format!("waiting for a worker that provides {s}"));
        self.systems = systems;
        announce
    }

    fn unbuilt(&self, path: &str, evaluation: &MEvaluation) -> BuildFailure {
        let mut message = format!(
            "build of {path} stopped because Gradient evaluation {} ended ({:?})",
            evaluation.id, evaluation.status
        );
        if let Some(systems) = &self.systems {
            message.push_str(&format!(
                " while waiting for a worker that provides {systems}"
            ));
        }

        BuildFailure {
            status: FailureStatus::MiscFailure,
            message,
        }
    }
}

fn missing_systems(evaluation: &MEvaluation) -> Option<String> {
    match WaitingReason::from_json(evaluation.waiting_reason.as_ref()?)? {
        WaitingReason::Workers { unmet, .. } if !unmet.is_empty() => Some(
            unmet
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        ),
        _ => None,
    }
}

pub async fn wait(
    session: &Session,
    started: &Started,
    log: impl Fn(String) + Send + Sync,
) -> anyhow::Result<BuildOutcome> {
    let state = &session.state;
    let derivations = closure_derivations(session, &started.closure).await?;
    let jobs: Vec<MBuildJob> = EBuildJob::find()
        .filter(CBuildJob::Evaluation.eq(started.evaluation))
        .all(&state.web_db)
        .await?
        .into_iter()
        .filter(|j| derivations.contains_key(&j.derivation))
        .collect();
    let build_of: HashMap<String, DerivationBuildId> = jobs
        .iter()
        .filter_map(|j| {
            let path = derivations.get(&j.derivation)?.store_path();
            Some((path, j.derivation_build))
        })
        .collect();
    let prefixes: HashMap<DerivationBuildId, String> = jobs
        .iter()
        .filter_map(|j| {
            let d = derivations.get(&j.derivation)?;
            Some((
                j.derivation_build,
                d.pname.clone().unwrap_or(d.name.clone()),
            ))
        })
        .collect();
    let requested: Vec<DerivationBuildId> = started
        .requested
        .iter()
        .filter_map(|(path, _)| build_of.get(path).copied())
        .collect();
    let mut followed: HashMap<DerivationBuildId, Followed> = HashMap::new();
    let mut open: Vec<DerivationBuildId> = jobs.iter().map(|j| j.derivation_build).collect();
    let mut waiting = Waiting::default();

    let evaluation = loop {
        let evaluation = EEvaluation::find_by_id(started.evaluation)
            .one(&state.web_db)
            .await?
            .ok_or_else(|| anyhow::anyhow!("evaluation {} disappeared", started.evaluation))?;
        if let Some(line) = waiting.observe(&evaluation) {
            log(line);
        }

        open = follow_logs(session, open, &prefixes, &mut followed, &log).await?;
        if EvaluationStatus::TERMINAL.contains(&evaluation.status)
            || settled(session, &requested).await?
        {
            break evaluation;
        }

        tokio::time::sleep(POLL).await;
    };

    let mut results = Vec::with_capacity(started.requested.len());
    for (path, drv) in &started.requested {
        let build = build_of.get(path).copied();
        let unbuilt = waiting.unbuilt(path, &evaluation);
        let result = requested_result(session, build, path, drv, unbuilt).await?;
        results.push((path.clone(), result));
    }

    Ok(BuildOutcome { results })
}

async fn closure_derivations(
    session: &Session,
    closure: &[String],
) -> anyhow::Result<HashMap<DerivationId, MDerivation>> {
    let paths: HashSet<&str> = closure.iter().map(String::as_str).collect();
    let hashes: Vec<String> = closure
        .iter()
        .filter_map(|p| {
            strip_nix_store_prefix(p)
                .split_once('-')
                .map(|(h, _)| h.to_string())
        })
        .collect();
    Ok(gradient_db::fetch_in_chunks(&hashes, |chunk| async {
        EDerivation::find()
            .filter(CDerivation::Hash.is_in(chunk))
            .all(&session.state.web_db)
            .await
    })
    .await?
    .into_iter()
    .filter(|d| paths.contains(d.store_path().as_str()))
    .map(|d| (d.id, d))
    .collect())
}

async fn settled(session: &Session, requested: &[DerivationBuildId]) -> anyhow::Result<bool> {
    Ok(EDerivationBuild::find()
        .filter(CDerivationBuild::Id.is_in(requested.to_vec()))
        .all(&session.state.web_db)
        .await?
        .iter()
        .all(|b| settles_the_request(b.status)))
}

async fn follow_logs(
    session: &Session,
    open: Vec<DerivationBuildId>,
    prefixes: &HashMap<DerivationBuildId, String>,
    followed: &mut HashMap<DerivationBuildId, Followed>,
    log: &(impl Fn(String) + Send + Sync),
) -> anyhow::Result<Vec<DerivationBuildId>> {
    let state = &session.state;
    let builds = gradient_db::fetch_in_chunks(&open, |chunk| async {
        EDerivationBuild::find()
            .filter(CDerivationBuild::Id.is_in(chunk))
            .all(&state.web_db)
            .await
    })
    .await?;

    let mut still_open = Vec::new();
    for build in builds {
        let terminal = !matches!(
            build.status,
            BuildStatus::Created | BuildStatus::Queued | BuildStatus::Building
        );
        if build.status == BuildStatus::Building || (terminal && followed.contains_key(&build.id)) {
            let entry = followed.entry(build.id).or_insert_with(|| Followed {
                offset: 0,
                lines: PrefixedLines::new(prefixes.get(&build.id).cloned().unwrap_or_default()),
            });
            emit_new_lines(session, build.id, entry, terminal, log).await?;
        }

        if !terminal {
            still_open.push(build.id);
        }
    }

    Ok(still_open)
}

async fn emit_new_lines(
    session: &Session,
    build: DerivationBuildId,
    followed: &mut Followed,
    terminal: bool,
    log: &(impl Fn(String) + Send + Sync),
) -> anyhow::Result<()> {
    let state = &session.state;
    let Some(attempt) =
        gradient_db::scheduling::build_attempt::latest_attempt_id(&state.web_db, build).await?
    else {
        return Ok(());
    };

    let text = state.log_storage.read(attempt).await.unwrap_or_default();
    if let Some(new) = text.get(followed.offset..) {
        followed.offset = text.len();
        for line in followed.lines.push(new) {
            log(line);
        }
    }

    if terminal && let Some(rest) = followed.lines.finish() {
        log(rest);
    }

    Ok(())
}

async fn requested_result(
    session: &Session,
    build: Option<DerivationBuildId>,
    path: &str,
    drv: &Derivation,
    unbuilt: BuildFailure,
) -> anyhow::Result<DrvResult> {
    let outputs: Outputs = drv
        .outputs
        .iter()
        .map(|o| (o.name.clone(), o.path.clone()))
        .collect();
    let state = &session.state;
    let Some(build_id) = build else {
        return Ok(Ok(outputs));
    };

    let Some(build) = EDerivationBuild::find_by_id(build_id)
        .one(&state.web_db)
        .await?
    else {
        return Ok(Ok(outputs));
    };

    if build.status.is_terminal_success() {
        return Ok(Ok(outputs));
    }

    if !build.status.is_terminal_failure() {
        return Ok(Err(unbuilt));
    }

    let tail =
        match gradient_db::scheduling::build_attempt::latest_attempt_id(&state.web_db, build_id)
            .await?
        {
            Some(attempt) => {
                let text = state.log_storage.read(attempt).await.unwrap_or_default();
                let lines: Vec<&str> = text.lines().collect();
                lines[lines.len().saturating_sub(FAILURE_LINES)..].join("\n")
            }
            None => String::new(),
        };

    Ok(Err(BuildFailure {
        status: failure_status(build.status),
        message: format!("build of {path} failed ({:?})\n{tail}", build.status),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_status_matches_nix() {
        assert_eq!(
            failure_status(BuildStatus::FailedTimeout),
            FailureStatus::TimedOut
        );
        assert_eq!(
            failure_status(BuildStatus::DependencyFailed),
            FailureStatus::DependencyFailed
        );
        assert_eq!(
            failure_status(BuildStatus::FailedPermanent),
            FailureStatus::PermanentFailure
        );
        assert_eq!(
            failure_status(BuildStatus::FailedTransient),
            FailureStatus::TransientFailure
        );
        assert_eq!(
            failure_status(BuildStatus::Aborted),
            FailureStatus::MiscFailure
        );
    }

    #[test]
    fn an_aborted_evaluation_names_the_workers_it_waited_for() {
        let unmet = UnmetRequirement {
            architecture: "aarch64-linux".into(),
            required_features: vec![],
            build_count: 1,
        };
        let parked = MEvaluation {
            status: EvaluationStatus::Waiting,
            waiting_reason: Some(WaitingReason::workers(vec![unmet], 0, vec![]).to_json()),
            ..Default::default()
        };
        let aborted = MEvaluation {
            status: EvaluationStatus::Aborted,
            waiting_reason: None,
            ..parked.clone()
        };
        let mut waiting = Waiting::default();

        let line = waiting.observe(&parked).expect("the park is announced");
        assert!(line.contains("aarch64-linux"), "{line}");
        assert_eq!(waiting.observe(&parked), None);
        assert_eq!(waiting.observe(&aborted), None);

        let failure = waiting.unbuilt("/nix/store/a.drv", &aborted);
        assert_eq!(failure.status, FailureStatus::MiscFailure);
        assert!(failure.message.contains("Aborted"), "{}", failure.message);
        assert!(
            failure.message.contains("aarch64-linux"),
            "{}",
            failure.message
        );
    }

    #[test]
    fn only_a_final_build_status_answers_the_request() {
        for status in [
            BuildStatus::Completed,
            BuildStatus::Substituted,
            BuildStatus::FailedPermanent,
            BuildStatus::FailedTimeout,
            BuildStatus::DependencyFailed,
        ] {
            assert!(settles_the_request(status), "{status:?}");
        }

        for status in [
            BuildStatus::Created,
            BuildStatus::Queued,
            BuildStatus::Building,
            BuildStatus::FailedTransient,
            BuildStatus::Aborted,
        ] {
            assert!(!settles_the_request(status), "{status:?}");
        }
    }
}
