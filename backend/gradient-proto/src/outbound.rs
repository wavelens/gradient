/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use gradient_core::ServerState;
use gradient_entity::{base_worker, worker_registration};
use gradient_types::ids::ProjectId;
use gradient_types::{EBaseWorker, EWorkerRegistration};
use gradient_util::supervision::ChildSpec;
use gradient_wire::session::handshake::DialerCredentials;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::handler::{SessionOrigin, SessionsHandle, handle_socket};
use gradient_scheduler::Scheduler;
use gradient_scheduler::connection_failures::ConnectionDirection;

const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
const NOT_ENABLED_BY_ANY_PROJECT: &str = "base worker not enabled by any project";
const UNDECRYPTABLE: &str =
    "stored connection token cannot be decrypted with the current crypt key";

type Connecting = Arc<Mutex<HashSet<String>>>;

pub(crate) struct DialTarget {
    pub url: String,
    pub credentials: DialerCredentials,
}

struct DialRow {
    worker_id: String,
    url: String,
    token_peers: Vec<String>,
    token_encrypted: Option<String>,
    idle_reason: Option<&'static str>,
}

enum PlannedDial {
    Dial(DialTarget),
    Skip { worker_id: String, reason: String },
}

struct WorkerDial {
    worker_id: String,
    url: String,
    rows: Vec<DialRow>,
}

impl DialRow {
    fn from_registration(reg: worker_registration::Model) -> Self {
        Self {
            token_peers: vec![reg.peer_id.to_string()],
            worker_id: reg.worker_id,
            url: reg.url.unwrap_or_default(),
            token_encrypted: reg.token_encrypted,
            idle_reason: None,
        }
    }

    fn from_base_worker(bw: base_worker::Model, projects: Vec<ProjectId>) -> Self {
        let token_peers = match bw.authorize_against {
            Some(identity) => vec![identity.to_string()],
            None => projects.iter().map(ToString::to_string).collect(),
        };
        Self {
            idle_reason: projects.is_empty().then_some(NOT_ENABLED_BY_ANY_PROJECT),
            worker_id: bw.worker_id,
            url: bw.url.unwrap_or_default(),
            token_peers,
            token_encrypted: bw.token_encrypted,
        }
    }
}

pub fn start_outbound_loop(scheduler: Arc<Scheduler>, sessions: Arc<SessionsHandle>) {
    let connecting: Connecting = Arc::default();
    let pass_scheduler = Arc::clone(&scheduler);
    scheduler.state.shutdown.supervise(ChildSpec::periodic(
        "outbound-connect",
        Duration::from_secs(15),
        Duration::from_secs(60),
        move || {
            let scheduler = Arc::clone(&pass_scheduler);
            let sessions = Arc::clone(&sessions);
            let connecting = Arc::clone(&connecting);
            async move {
                connect_to_registered_workers(&scheduler, &sessions, &connecting).await;
                Ok(())
            }
        },
    ));
}

async fn connect_to_registered_workers(
    scheduler: &Arc<Scheduler>,
    sessions: &Arc<SessionsHandle>,
    connecting: &Connecting,
) {
    let state = &scheduler.state;
    let rows = match dial_rows(state).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "failed to query workers for outbound connections");
            return;
        }
    };

    let crypt_file = &state.config.secrets.crypt_file;
    let decrypt = |encrypted: &str| gradient_sources::decrypt_secret(crypt_file, encrypted).ok();
    for planned in plan_dials(rows, decrypt) {
        match planned {
            PlannedDial::Dial(target) => start_dial(scheduler, sessions, connecting, target).await,
            PlannedDial::Skip { worker_id, reason } => {
                debug!(%worker_id, %reason, "not dialing worker");
                scheduler.connection_failures.record(
                    &worker_id,
                    ConnectionDirection::Outbound,
                    false,
                    reason,
                );
            }
        }
    }
}

async fn dial_rows(state: &ServerState) -> Result<Vec<DialRow>, sea_orm::DbErr> {
    let registrations = EWorkerRegistration::find()
        .filter(worker_registration::Column::Url.is_not_null())
        .filter(worker_registration::Column::Active.eq(true))
        .order_by_asc(worker_registration::Column::CreatedAt)
        .all(&state.worker_db)
        .await?;
    let registered: HashSet<String> = registrations.iter().map(|r| r.worker_id.clone()).collect();
    let base_workers = EBaseWorker::find()
        .filter(base_worker::Column::Url.is_not_null())
        .filter(base_worker::Column::Enabled.eq(true))
        .order_by_asc(base_worker::Column::CreatedAt)
        .all(&state.worker_db)
        .await?;

    let mut rows: Vec<DialRow> = registrations
        .into_iter()
        .map(DialRow::from_registration)
        .collect();
    for bw in base_workers
        .into_iter()
        .filter(|bw| !registered.contains(&bw.worker_id))
    {
        let projects = gradient_db::projects::base_workers::projects_enabling_base_worker(
            &state.worker_db,
            bw.id,
        )
        .await?;
        rows.push(DialRow::from_base_worker(bw, projects));
    }

    Ok(rows)
}

fn plan_dials(rows: Vec<DialRow>, decrypt: impl Fn(&str) -> Option<String>) -> Vec<PlannedDial> {
    group_by_worker(rows)
        .into_iter()
        .map(|group| plan_worker(group, &decrypt))
        .collect()
}

fn group_by_worker(rows: Vec<DialRow>) -> Vec<WorkerDial> {
    let mut groups: Vec<WorkerDial> = Vec::new();
    for row in rows.into_iter().filter(|r| !r.url.is_empty()) {
        match groups.iter_mut().find(|g| g.worker_id == row.worker_id) {
            Some(group) if group.url == row.url => group.rows.push(row),
            Some(_) => {}
            None => groups.push(WorkerDial {
                worker_id: row.worker_id.clone(),
                url: row.url.clone(),
                rows: vec![row],
            }),
        }
    }

    groups
}

fn plan_worker(group: WorkerDial, decrypt: &impl Fn(&str) -> Option<String>) -> PlannedDial {
    let WorkerDial {
        worker_id,
        url,
        rows,
    } = group;
    if let Some(reason) = rows.iter().find_map(|r| r.idle_reason) {
        return PlannedDial::Skip {
            worker_id,
            reason: reason.into(),
        };
    }

    let mut tokens = Vec::new();
    for row in &rows {
        let Some(encrypted) = &row.token_encrypted else {
            continue;
        };
        let Some(token) = decrypt(encrypted) else {
            return PlannedDial::Skip {
                worker_id,
                reason: UNDECRYPTABLE.into(),
            };
        };
        tokens.extend(
            row.token_peers
                .iter()
                .map(|peer| (peer.clone(), token.clone())),
        );
    }

    PlannedDial::Dial(DialTarget {
        credentials: DialerCredentials {
            worker_id,
            tokens: tokens_for_scheme(&url, tokens),
        },
        url,
    })
}

fn tokens_for_scheme(url: &str, tokens: Vec<(String, String)>) -> Vec<(String, String)> {
    if !url.starts_with("wss://") && !tokens.is_empty() {
        warn!(%url, "sending connection tokens over an unencrypted ws:// connection");
    }
    tokens
}

async fn start_dial(
    scheduler: &Arc<Scheduler>,
    sessions: &Arc<SessionsHandle>,
    connecting: &Connecting,
    target: DialTarget,
) {
    let worker_id = target.credentials.worker_id.clone();
    if scheduler.is_worker_connected(&worker_id).await
        || !connecting.lock().await.insert(worker_id.clone())
    {
        return;
    }

    let scheduler = Arc::clone(scheduler);
    let sessions = Arc::clone(sessions);
    let connecting = Arc::clone(connecting);
    let shutdown = scheduler.state.shutdown.clone();
    shutdown.spawn(async move {
        dial(&scheduler, &sessions, target).await;
        connecting.lock().await.remove(&worker_id);
    });
}

async fn dial(scheduler: &Arc<Scheduler>, sessions: &Arc<SessionsHandle>, target: DialTarget) {
    let worker_id = target.credentials.worker_id.clone();
    let url = target.url.clone();
    debug!(%worker_id, %url, "connecting outbound to worker");

    match tokio::time::timeout(DIAL_TIMEOUT, gradient_wire::client::dial(&url)).await {
        Ok(Ok(socket)) => {
            info!(%worker_id, %url, "outbound connection established");
            handle_socket(
                socket,
                Arc::clone(&scheduler.state),
                Arc::clone(scheduler),
                Arc::clone(sessions),
                SessionOrigin::ServerDialed(target),
            )
            .await;
            info!(%worker_id, "outbound connection closed");
        }
        Ok(Err(e)) => {
            error!(%worker_id, %url, error = %e, "outbound connection failed");
            scheduler.connection_failures.record(
                &worker_id,
                ConnectionDirection::Outbound,
                false,
                format!("dial failed: {e:#}"),
            );
        }
        Err(_) => {
            error!(%worker_id, %url, "outbound connection timed out (10s)");
            scheduler.connection_failures.record(
                &worker_id,
                ConnectionDirection::Outbound,
                false,
                "dial timed out after 10 s",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registration(worker_id: &str, peer: &str, url: &str, token: Option<&str>) -> DialRow {
        DialRow {
            worker_id: worker_id.into(),
            url: url.into(),
            token_peers: vec![peer.into()],
            token_encrypted: token.map(|t| format!("enc:{t}")),
            idle_reason: None,
        }
    }

    fn plain(encrypted: &str) -> Option<String> {
        encrypted.strip_prefix("enc:").map(str::to_owned)
    }

    fn only_dial(planned: Vec<PlannedDial>) -> DialTarget {
        let Some([PlannedDial::Dial(target)]) = <[PlannedDial; 1]>::try_from(planned).ok() else {
            panic!("expected exactly one dial");
        };
        target
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(p, t)| (p.to_string(), t.to_string()))
            .collect()
    }

    #[test]
    fn two_projects_sharing_a_worker_are_dialed_once_with_both_tokens() {
        let target = only_dial(plan_dials(
            vec![
                registration("w1", "p1", "wss://w1.example/proto", Some("t1")),
                registration("w1", "p2", "wss://w1.example/proto", Some("t2")),
            ],
            plain,
        ));

        assert_eq!(target.url, "wss://w1.example/proto");
        assert_eq!(
            target.credentials.tokens,
            pairs(&[("p1", "t1"), ("p2", "t2")])
        );
    }

    #[test]
    fn a_second_url_for_the_same_worker_gets_no_token() {
        let target = only_dial(plan_dials(
            vec![
                registration("w1", "p1", "wss://w1.example/proto", Some("t1")),
                registration("w1", "p2", "wss://elsewhere.example/proto", Some("t2")),
            ],
            plain,
        ));

        assert_eq!(target.url, "wss://w1.example/proto");
        assert_eq!(target.credentials.tokens, pairs(&[("p1", "t1")]));
    }

    #[test]
    fn an_undecryptable_token_skips_the_dial_with_a_reason() {
        let planned = plan_dials(
            vec![registration(
                "w1",
                "p1",
                "wss://w1.example/proto",
                Some("t1"),
            )],
            |_| None,
        );

        assert!(matches!(
            planned.as_slice(),
            [PlannedDial::Skip { reason, .. }] if reason.as_str() == UNDECRYPTABLE
        ));
    }

    #[test]
    fn a_plain_websocket_still_carries_the_tokens() {
        let target = only_dial(plan_dials(
            vec![registration("w1", "p1", "ws://w1.local/proto", Some("t1"))],
            plain,
        ));

        assert_eq!(target.credentials.tokens, pairs(&[("p1", "t1")]));
    }

    #[test]
    fn a_registration_without_a_stored_token_is_still_dialed() {
        let target = only_dial(plan_dials(
            vec![registration("w1", "p1", "wss://w1.example/proto", None)],
            plain,
        ));

        assert_eq!(target.credentials.worker_id, "w1");
        assert!(target.credentials.tokens.is_empty());
    }

    #[test]
    fn a_base_worker_without_enabled_projects_is_not_dialed() {
        let base = base_worker::Model {
            worker_id: "b1".into(),
            url: Some("wss://b1.example/proto".into()),
            ..Default::default()
        };
        let planned = plan_dials(vec![DialRow::from_base_worker(base, vec![])], plain);

        assert!(matches!(
            planned.as_slice(),
            [PlannedDial::Skip { reason, .. }] if reason.as_str() == NOT_ENABLED_BY_ANY_PROJECT
        ));
    }

    #[test]
    fn the_dial_carries_the_decrypted_token() {
        let crypt = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(crypt.path(), "a-32-byte-crypt-key-for-the-test").unwrap();
        let path = crypt.path().to_string_lossy().into_owned();
        let row = DialRow {
            token_encrypted: Some(gradient_sources::encrypt_secret(&path, "s3cret").unwrap()),
            ..registration("w1", "p1", "wss://w1.example/proto", None)
        };

        let target = only_dial(plan_dials(vec![row], |enc| {
            gradient_sources::decrypt_secret(&path, enc).ok()
        }));
        assert_eq!(target.credentials.tokens, pairs(&[("p1", "s3cret")]));
    }
}
