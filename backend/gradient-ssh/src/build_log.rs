/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::build_request::drv_name;
use crate::build_wait::settles_the_request;
use crate::session::Session;
use gradient_entity::build::BuildStatus;
use gradient_types::*;
use harmonia_protocol::log::{
    Activity, ActivityResult, ActivityType, Field, LogMessage, ResultType, StopActivity, Verbosity,
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ACTIVITY: AtomicU64 = AtomicU64::new(0);

fn activity_id() -> u64 {
    ((std::process::id() as u64) << 32) + NEXT_ACTIVITY.fetch_add(1, Ordering::Relaxed)
}

pub struct BuildActivity {
    id: u64,
    attempt: Option<BuildAttemptId>,
    offset: usize,
    pending: String,
}

impl BuildActivity {
    pub fn start(drv_path: &str) -> (Self, LogMessage) {
        let id = activity_id();
        let start = LogMessage::StartActivity(Activity {
            id,
            level: Verbosity::Info,
            parent: 0,
            text: format!("building '{drv_path}'").into(),
            activity_type: ActivityType::Build,
            fields: vec![
                Field::String(drv_path.to_owned().into()),
                Field::String(Default::default()),
                Field::Int(1),
                Field::Int(1),
            ],
        });
        let activity = Self {
            id,
            attempt: None,
            offset: 0,
            pending: String::new(),
        };
        (activity, start)
    }

    pub fn read(&mut self, attempt: BuildAttemptId, log: &str) -> Vec<LogMessage> {
        let mut lines = Vec::new();
        if self.attempt != Some(attempt) {
            lines.extend(self.rest());
            self.attempt = Some(attempt);
            self.offset = 0;
        }

        if let Some(new) = log.get(self.offset..) {
            self.offset = log.len();
            self.pending.push_str(new);
        }

        if let Some(end) = self.pending.rfind('\n') {
            let complete: String = self.pending.drain(..=end).collect();
            lines.extend(complete.lines().map(|line| self.line(line)));
        }

        lines
    }

    pub fn stop(mut self) -> Vec<LogMessage> {
        let mut messages: Vec<LogMessage> = self.rest().into_iter().collect();
        messages.push(LogMessage::StopActivity(StopActivity { id: self.id }));
        messages
    }

    fn rest(&mut self) -> Option<LogMessage> {
        let rest = std::mem::take(&mut self.pending);
        (!rest.is_empty()).then(|| self.line(&rest))
    }

    fn line(&self, line: &str) -> LogMessage {
        LogMessage::Result(ActivityResult {
            id: self.id,
            result_type: ResultType::BuildLogLine,
            fields: vec![Field::String(line.to_owned().into())],
        })
    }
}

pub struct BuildLogs {
    drv_paths: HashMap<DerivationBuildId, String>,
    open: Vec<DerivationBuildId>,
    activities: HashMap<DerivationBuildId, BuildActivity>,
}

impl BuildLogs {
    pub async fn unsettled(
        session: &Session,
        drv_paths: HashMap<DerivationBuildId, String>,
    ) -> anyhow::Result<Self> {
        let ids: Vec<DerivationBuildId> = drv_paths.keys().copied().collect();
        let open = builds(session, &ids)
            .await?
            .into_iter()
            .filter(|b| !settles_the_request(b.status))
            .map(|b| b.id)
            .collect();
        Ok(Self {
            drv_paths,
            open,
            activities: HashMap::new(),
        })
    }

    pub async fn follow(
        &mut self,
        session: &Session,
        log: &(impl Fn(LogMessage) + Send + Sync),
    ) -> anyhow::Result<()> {
        let mut open = Vec::new();
        for build in builds(session, &self.open).await? {
            let settled = settles_the_request(build.status);
            if build.status == BuildStatus::Building || settled {
                self.read(session, build.id, log).await?;
            }

            if !settled {
                open.push(build.id);
            } else if let Some(activity) = self.activities.remove(&build.id) {
                activity.stop().into_iter().for_each(log);
            }
        }

        self.open = open;
        Ok(())
    }

    pub fn finish(self, log: &(impl Fn(LogMessage) + Send + Sync)) {
        for activity in self.activities.into_values() {
            activity.stop().into_iter().for_each(log);
        }
    }

    async fn read(
        &mut self,
        session: &Session,
        build: DerivationBuildId,
        log: &(impl Fn(LogMessage) + Send + Sync),
    ) -> anyhow::Result<()> {
        let state = &session.state;
        let Some(attempt) =
            gradient_db::scheduling::build_attempt::latest_attempt_id(&state.web_db, build).await?
        else {
            return Ok(());
        };

        let drv_path = self.drv_paths.get(&build).map_or("", String::as_str);
        let activity = self.activities.entry(build).or_insert_with(|| {
            let (activity, start) = BuildActivity::start(drv_path);
            log(start);
            activity
        });
        let text = state.log_storage.read(attempt).await.unwrap_or_default();
        activity.read(attempt, &text).into_iter().for_each(log);
        Ok(())
    }
}

#[derive(Default)]
pub struct PlainLog {
    names: HashMap<u64, String>,
}

impl PlainLog {
    pub fn line(&mut self, message: LogMessage) -> Option<String> {
        match message {
            LogMessage::Message(m) => Some(String::from_utf8_lossy(&m.text).into_owned()),
            LogMessage::StartActivity(a) => {
                if let Some(Field::String(path)) = a.fields.first() {
                    let name = drv_name(&String::from_utf8_lossy(path)).to_owned();
                    self.names.insert(a.id, name);
                }

                Some(String::from_utf8_lossy(&a.text).into_owned())
            }
            LogMessage::Result(r) if r.result_type == ResultType::BuildLogLine => {
                let Some(Field::String(line)) = r.fields.first() else {
                    return None;
                };
                let name = self.names.get(&r.id).map_or("", String::as_str);
                Some(format!("{name}> {}", String::from_utf8_lossy(line)))
            }
            LogMessage::Result(_) => None,
            LogMessage::StopActivity(s) => {
                self.names.remove(&s.id);
                None
            }
        }
    }
}

async fn builds(
    session: &Session,
    ids: &[DerivationBuildId],
) -> anyhow::Result<Vec<MDerivationBuild>> {
    Ok(gradient_db::fetch_in_chunks(ids, |chunk| async {
        EDerivationBuild::find()
            .filter(CDerivationBuild::Id.is_in(chunk))
            .all(&session.state.web_db)
            .await
    })
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Mutex;

    const DRV: &str = "/nix/store/00000000000000000000000000000000-hello.drv";

    fn log_lines(id: u64, messages: &[LogMessage]) -> Vec<String> {
        messages
            .iter()
            .filter_map(|m| match m {
                LogMessage::Result(r) if r.result_type == ResultType::BuildLogLine => {
                    assert_eq!(r.id, id);
                    match r.fields.as_slice() {
                        [Field::String(line)] => Some(String::from_utf8_lossy(line).into_owned()),
                        other => panic!("a log line carries one string field: {other:?}"),
                    }
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_build_log_reaches_nix_as_the_lines_of_one_build_activity() {
        let (mut activity, start) = BuildActivity::start(DRV);
        let LogMessage::StartActivity(started) = start else {
            panic!("the activity starts first: {start:?}");
        };
        assert_eq!(started.activity_type, ActivityType::Build);
        assert_eq!(
            started.fields,
            [
                Field::String(DRV.into()),
                Field::String(Default::default()),
                Field::Int(1),
                Field::Int(1),
            ]
        );

        let first = BuildAttemptId::now_v7();
        assert_eq!(
            log_lines(started.id, &activity.read(first, "one\ntw")),
            ["one"]
        );
        assert_eq!(
            log_lines(started.id, &activity.read(first, "one\ntwo\n")),
            ["two"]
        );
        let retry = BuildAttemptId::now_v7();
        assert!(activity.read(retry, "again").is_empty());

        let stopped = activity.stop();
        assert_eq!(log_lines(started.id, &stopped), ["again"]);
        assert!(
            matches!(stopped.last(), Some(LogMessage::StopActivity(s)) if s.id == started.id),
            "{stopped:?}"
        );
    }

    #[test]
    fn plain_text_puts_the_package_name_in_front_of_each_line() {
        let (mut activity, start) = BuildActivity::start(DRV);
        let mut messages = vec![start];
        messages.extend(activity.read(BuildAttemptId::now_v7(), "compiling\n"));
        messages.extend(activity.stop());
        let mut plain = PlainLog::default();

        let lines: Vec<String> = messages.into_iter().filter_map(|m| plain.line(m)).collect();
        assert_eq!(
            lines,
            [format!("building '{DRV}'"), "hello> compiling".into()]
        );
        assert!(plain.names.is_empty());
    }

    #[tokio::test]
    async fn a_build_that_finished_between_two_polls_still_opens_and_closes_its_activity() {
        let build = MDerivationBuild {
            id: DerivationBuildId::now_v7(),
            status: BuildStatus::Completed,
            ..Default::default()
        };
        let attempt = MBuildAttempt {
            id: BuildAttemptId::now_v7(),
            derivation_build: build.id,
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![build.clone()]])
            .append_query_results([vec![attempt]])
            .into_connection();
        let session = Session {
            state: gradient_test_support::state::test_state_web(db),
            user: gradient_test_support::fixtures::user(),
            project: gradient_test_support::fixtures::project(),
            permissions: 0,
            caches: vec![],
            closed: Default::default(),
        };
        let mut logs = BuildLogs {
            drv_paths: HashMap::from([(build.id, DRV.to_owned())]),
            open: vec![build.id],
            activities: HashMap::new(),
        };
        let sent = Mutex::new(Vec::new());

        logs.follow(&session, &|m| sent.lock().expect("sent").push(m))
            .await
            .expect("follow");

        let sent = sent.into_inner().expect("sent");
        assert!(
            matches!(sent.as_slice(), [LogMessage::StartActivity(a), LogMessage::StopActivity(s)] if a.id == s.id),
            "{sent:?}"
        );
        assert!(logs.open.is_empty());
        assert!(logs.activities.is_empty());
    }
}
