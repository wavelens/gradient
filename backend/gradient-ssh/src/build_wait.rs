/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::session::Session;
use gradient_derivation::Derivation;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use gradient_util::store_path::strip_nix_store_prefix;
use harmonia_protocol::daemon::wire::types2::FailureStatus;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::collections::{BTreeMap, HashMap};
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
    prefix: String,
    offset: usize,
    pending: String,
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

pub fn split_lines(prefix: &str, pending: &mut String, chunk: &str) -> Vec<String> {
    pending.push_str(chunk);
    let Some(end) = pending.rfind('\n') else {
        return Vec::new();
    };

    let complete: String = pending.drain(..=end).collect();
    complete
        .lines()
        .map(|line| format!("{prefix}> {line}"))
        .collect()
}

pub async fn wait(
    session: &Session,
    evaluation: EvaluationId,
    requested: &[(String, Derivation)],
    log: impl Fn(String) + Send + Sync,
) -> anyhow::Result<BuildOutcome> {
    let state = &session.state;
    let jobs = EBuildJob::find()
        .filter(CBuildJob::Evaluation.eq(evaluation))
        .all(&state.web_db)
        .await?;
    let build_of: HashMap<DerivationId, DerivationBuildId> = jobs
        .iter()
        .map(|j| (j.derivation, j.derivation_build))
        .collect();
    let prefixes = prefixes(session, &jobs).await?;
    let mut followed: HashMap<DerivationBuildId, Followed> = HashMap::new();
    let mut open: Vec<DerivationBuildId> = jobs.iter().map(|j| j.derivation_build).collect();

    loop {
        let status = EEvaluation::find_by_id(evaluation)
            .one(&state.web_db)
            .await?
            .ok_or_else(|| anyhow::anyhow!("evaluation {evaluation} disappeared"))?
            .status;

        open = follow_logs(session, open, &prefixes, &mut followed, &log).await?;
        if EvaluationStatus::TERMINAL.contains(&status) {
            break;
        }

        tokio::time::sleep(POLL).await;
    }

    let mut results = Vec::with_capacity(requested.len());
    for (path, drv) in requested {
        let result = requested_result(session, &build_of, path, drv).await?;
        results.push((path.clone(), result));
    }

    Ok(BuildOutcome { results })
}

async fn prefixes(
    session: &Session,
    jobs: &[MBuildJob],
) -> anyhow::Result<HashMap<DerivationBuildId, String>> {
    let ids: Vec<DerivationId> = jobs.iter().map(|j| j.derivation).collect();
    let names: HashMap<DerivationId, String> = gradient_db::fetch_in_chunks(&ids, |chunk| async {
        EDerivation::find()
            .filter(CDerivation::Id.is_in(chunk))
            .all(&session.state.web_db)
            .await
    })
    .await?
    .into_iter()
    .map(|d| (d.id, d.pname.unwrap_or(d.name)))
    .collect();

    Ok(jobs
        .iter()
        .map(|j| {
            let name = names.get(&j.derivation).cloned().unwrap_or_default();
            (j.derivation_build, name)
        })
        .collect())
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
                prefix: prefixes.get(&build.id).cloned().unwrap_or_default(),
                offset: 0,
                pending: String::new(),
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
        for line in split_lines(&followed.prefix, &mut followed.pending, new) {
            log(line);
        }
    }

    if terminal && !followed.pending.is_empty() {
        log(format!(
            "{}> {}",
            followed.prefix,
            std::mem::take(&mut followed.pending)
        ));
    }

    Ok(())
}

async fn requested_result(
    session: &Session,
    build_of: &HashMap<DerivationId, DerivationBuildId>,
    path: &str,
    drv: &Derivation,
) -> anyhow::Result<DrvResult> {
    let outputs: Outputs = drv
        .outputs
        .iter()
        .map(|o| (o.name.clone(), o.path.clone()))
        .collect();
    let state = &session.state;
    let base = strip_nix_store_prefix(path);
    let (hash, name) = base.split_once('-').unwrap_or((&base, ""));
    let derivation = EDerivation::find()
        .filter(CDerivation::Hash.eq(hash))
        .filter(CDerivation::Name.eq(name.strip_suffix(".drv").unwrap_or(name)))
        .one(&state.web_db)
        .await?;

    let Some(build_id) = derivation.and_then(|d| build_of.get(&d.id).copied()) else {
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
    fn log_lines_carry_the_package_prefix() {
        let mut pending = String::new();
        let lines = split_lines("hello", &mut pending, "one\ntwo\nthr");
        assert_eq!(lines, ["hello> one", "hello> two"]);
        assert_eq!(pending, "thr");
    }
}
