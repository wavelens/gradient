/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;

use chrono::{Duration, NaiveDateTime, Utc};
use gradient_entity::build_attempt::AttemptOutcome;
use gradient_entity::cluster_job::ClusterJobStatus;
use gradient_entity::ids::*;
use gradient_entity::phase_event::PhaseSubjectKind;
use gradient_entity::{
    api, cached_path, cli_device_authorization, cluster_member, dispatched_job, exec_job,
    exec_job_attempt, exec_job_phase, exec_job_token, git_host_connection, phase_event, project,
    session, task, worker_connection, worker_sample,
};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::ids::user_id;
use gradient_fixtures::shape::shape;
use gradient_fixtures::test_date;
use gradient_report::{
    Row, TableSpec, eval_scope_tables, exec_tables, fetch_rows, instance_tables,
};
use sea_orm::prelude::Uuid;
use sea_orm::sqlx::{AssertSqlSafe, Column, Executor, SqlSafeStr, Statement};
use sea_orm::{ActiveModelTrait, IntoActiveModel};

fn drv(letter: char, name: &str) -> String {
    format!("{}-{name}.drv", letter.to_string().repeat(32))
}

fn hash(letter: char) -> String {
    letter.to_string().repeat(32)
}

fn spec_named(name: &str) -> &'static TableSpec {
    all_specs()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no spec {name}"))
}

fn all_specs() -> impl Iterator<Item = &'static TableSpec> {
    eval_scope_tables()
        .iter()
        .chain(exec_tables())
        .chain(instance_tables())
}

async fn exported(db: &TestDatabase, table: &str, scope: impl Into<Uuid>) -> Vec<Row> {
    fetch_rows(&db.connection(), spec_named(table), scope.into())
        .await
        .unwrap_or_else(|e| panic!("{table}: {e:#}"))
}

async fn exported_column(
    db: &TestDatabase,
    table: &str,
    column: &str,
    scope: impl Into<Uuid>,
) -> BTreeSet<String> {
    let at = spec_named(table)
        .columns
        .iter()
        .position(|c| *c == column)
        .unwrap_or_else(|| panic!("no column {table}.{column}"));
    exported(db, table, scope)
        .await
        .into_iter()
        .filter_map(|row| row[at].clone())
        .collect()
}

fn ids<I: ToString>(ids: impl IntoIterator<Item = I>) -> BTreeSet<String> {
    ids.into_iter().map(|id| id.to_string()).collect()
}

fn hours_ago(hours: i64) -> NaiveDateTime {
    Utc::now().naive_utc() - Duration::hours(hours)
}

struct Seeded {
    db: TestDatabase,
    evaluation: EvaluationId,
    other_evaluation: EvaluationId,
    clusters: [ClusterJobId; 4],
}

async fn seed_evaluation_with_dependencies_clusters_and_secrets() -> Seeded {
    let (evaluation, other_evaluation) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let clusters = [(); 4].map(|()| ClusterJobId::now_v7());
    let cache = CacheId::now_v7();
    let (a, b, c, d) = (drv('a', "a"), drv('b', "b"), drv('c', "c"), drv('d', "d"));
    let db = shape()
        .evaluation(evaluation)
        .derivation(&a)
        .output(&hash('1'))
        .job()
        .entry_point("a")
        .depends_on(&b)
        .cached_path(&hash('1'), "a")
        .into_cache(cache)
        .with_legacy_cached_path_before_producer_tracking()
        .derivation(&b)
        .output(&hash('2'))
        .depends_on(&c)
        .derivation(&c)
        .output(&hash('3'))
        .eval_assignment()
        .cluster(
            clusters[0],
            false,
            1,
            ClusterJobStatus::Queued,
            0,
            test_date(),
        )
        .cluster(
            clusters[1],
            false,
            1,
            ClusterJobStatus::Queued,
            0,
            test_date(),
        )
        .cluster(
            clusters[2],
            false,
            1,
            ClusterJobStatus::Queued,
            0,
            test_date(),
        )
        .cluster(
            clusters[3],
            false,
            1,
            ClusterJobStatus::Queued,
            0,
            test_date(),
        )
        .evaluation_member_of(clusters[0], "eval", true, None)
        .derivation(&a)
        .build_member_of(clusters[1], "build", true, None)
        .evaluation(other_evaluation)
        .derivation(&a)
        .job()
        .attempt(AttemptOutcome::Built, None)
        .derivation(&d)
        .output(&hash('4'))
        .job()
        .cached_path(&hash('4'), "d")
        .into_cache(cache)
        .with_legacy_cached_path_before_producer_tracking()
        .evaluation_member_of(clusters[3], "eval", true, None)
        .into_database()
        .await;

    let exec = ExecJobId::now_v7();
    let entry_point = db.entry_points(evaluation).await[0].id;
    let assignment = db.rows::<dispatched_job::Entity>().await[0].id;
    let attempt = ExecJobAttemptId::now_v7();
    let rows = (
        exec_job::Model {
            id: exec,
            evaluation,
            entry_point,
            program_derivation: db.id_of(&a).await,
            requirement: serde_json::json!({}),
            spec: serde_json::json!({
                "env": {"TOKEN": "exec-env-secret"},
                "args": ["--key=exec-arg-secret"],
                "image": "kept",
            }),
            created_at: test_date(),
            ..Default::default()
        },
        exec_job_attempt::Model {
            id: attempt,
            exec_job: exec,
            assignment_id: assignment,
            worker: "w".into(),
            started_at: test_date(),
            ..Default::default()
        },
        exec_job_phase::Model {
            id: ExecJobPhaseId::now_v7(),
            exec_job: exec,
            attempt,
            name: "run".into(),
            payload: b"phase-payload-secret".to_vec(),
            created_at: test_date(),
        },
        cluster_member::Model {
            id: ClusterMemberId::now_v7(),
            cluster_job: clusters[2],
            role: "exec".into(),
            primary: true,
            exec_job: Some(exec),
            ..Default::default()
        },
    );
    let conn = db.connection();
    rows.0
        .into_active_model()
        .insert(&conn)
        .await
        .expect("exec job");
    rows.1
        .into_active_model()
        .insert(&conn)
        .await
        .expect("attempt");
    rows.2
        .into_active_model()
        .insert(&conn)
        .await
        .expect("phase");
    rows.3
        .into_active_model()
        .insert(&conn)
        .await
        .expect("member");

    let own_build = db.shared_build(&a).await.id;
    let other_job = db.job(other_evaluation, &a).await.expect("job").id;
    for (kind, subject) in [
        (PhaseSubjectKind::Build, Uuid::from(own_build)),
        (PhaseSubjectKind::Build, Uuid::from(other_job)),
        (PhaseSubjectKind::Evaluation, Uuid::from(evaluation)),
    ] {
        phase_event::Model {
            id: PhaseEventId::now_v7(),
            subject_kind: kind,
            subject_id: subject,
            at: test_date(),
            ..Default::default()
        }
        .into_active_model()
        .insert(&conn)
        .await
        .expect("phase event");
    }

    Seeded {
        db,
        evaluation,
        other_evaluation,
        clusters,
    }
}

#[tokio::test]
async fn export_rows_include_dependency_build_job_phase_path_and_cluster_member_rows_and_exclude_unrelated_evaluations()
 {
    let Seeded {
        db,
        evaluation,
        other_evaluation,
        clusters,
    } = seed_evaluation_with_dependencies_clusters_and_secrets().await;
    let (a, b, c, d) = (drv('a', "a"), drv('b', "b"), drv('c', "c"), drv('d', "d"));
    let own = ids([db.id_of(&a).await, db.id_of(&b).await]);

    assert_eq!(
        exported_column(&db, "derivation", "id", evaluation).await,
        own
    );
    assert_eq!(
        exported_column(&db, "derivation_build", "derivation", evaluation).await,
        own
    );
    assert_eq!(
        exported_column(&db, "derivation_output", "derivation", evaluation).await,
        own
    );
    assert!(!own.contains(&db.id_of(&c).await.to_string()));
    assert!(!own.contains(&db.id_of(&d).await.to_string()));

    let jobs = ids([
        db.job(evaluation, &a).await.expect("own job").id,
        db.job(other_evaluation, &a).await.expect("other job").id,
    ]);
    assert_eq!(
        exported_column(&db, "build_job", "id", evaluation).await,
        jobs
    );

    let subjects = exported_column(&db, "phase_event", "subject_id", evaluation).await;
    assert_eq!(
        subjects,
        ids([
            Uuid::from(db.shared_build(&a).await.id),
            Uuid::from(evaluation)
        ])
    );

    assert_eq!(
        exported_column(&db, "cached_path", "hash", evaluation).await,
        ids([hash('1')])
    );
    let own_cached_path = db
        .rows::<cached_path::Entity>()
        .await
        .into_iter()
        .filter(|path| path.hash == hash('1'))
        .map(|path| path.id)
        .collect::<Vec<_>>();
    assert_eq!(own_cached_path.len(), 1);
    assert_eq!(
        exported_column(&db, "cached_path_signature", "cached_path", evaluation).await,
        ids(own_cached_path)
    );

    let exported_clusters = ids(clusters[..3].iter().copied());
    assert_eq!(
        exported_column(&db, "cluster_job", "id", evaluation).await,
        exported_clusters
    );
    assert_eq!(
        exported_column(&db, "cluster_member", "cluster_job", evaluation).await,
        exported_clusters
    );
    db.finish().await;
}

async fn seed_credential_rows(db: &TestDatabase) -> Vec<String> {
    let conn = db.connection();
    let owner = user_id();
    let now = Utc::now().naive_utc();
    let attempt = db.rows::<exec_job_attempt::Entity>().await[0].id;
    let project = db.rows::<project::Entity>().await[0].id;
    let task = db.rows::<task::Entity>().await[0].id;
    let session = SessionId::now_v7();

    api::Model {
        id: ApiId::now_v7(),
        owned_by: owner,
        name: "credential-api".into(),
        key: "credential-api-key".into(),
        last_used_at: now,
        created_at: now,
        ..Default::default()
    }
    .into_active_model()
    .insert(&conn)
    .await
    .expect("api key");
    session::Model {
        id: session,
        user_id: owner,
        created_at: now,
        expires_at: now,
        last_used_at: now,
        user_agent: Some("credential-user-agent".into()),
        ip: Some("credential-session-ip".into()),
        ..Default::default()
    }
    .into_active_model()
    .insert(&conn)
    .await
    .expect("session");
    cli_device_authorization::Model {
        id: CliDeviceAuthorizationId::now_v7(),
        device_code_hash: "credential-device-code-hash".into(),
        user_code: "credential-user-code".into(),
        user_id: Some(owner),
        approving_session_id: Some(session),
        created_at: now,
        expires_at: now,
        ..Default::default()
    }
    .into_active_model()
    .insert(&conn)
    .await
    .expect("device authorization");
    git_host_connection::Model {
        id: GitHostConnectionId::now_v7(),
        project: Some(project),
        name: "credential-connection".into(),
        display_name: "credential-connection".into(),
        secret: Some("credential-webhook-secret".into()),
        access_token: Some("credential-access-token".into()),
        encrypted_refresh_token: Some("credential-refresh-token".into()),
        created_by: owner,
        created_at: now,
        ..Default::default()
    }
    .into_active_model()
    .insert(&conn)
    .await
    .expect("git host connection");
    exec_job_token::Model {
        id: ExecJobTokenId::now_v7(),
        attempt,
        token_hash: "credential-exec-token-hash".into(),
        project,
        task,
        expires_at: now,
        created_at: now,
        ..Default::default()
    }
    .into_active_model()
    .insert(&conn)
    .await
    .expect("exec job token");

    [
        "credential-api-key",
        "credential-user-agent",
        "credential-session-ip",
        "credential-device-code-hash",
        "credential-user-code",
        "credential-webhook-secret",
        "credential-access-token",
        "credential-refresh-token",
        "credential-exec-token-hash",
        "legacy-signature",
        "exec-env-secret",
        "exec-arg-secret",
        "phase-payload-secret",
    ]
    .iter()
    .flat_map(|s| {
        let hex: String = s.bytes().map(|b| format!("{b:02x}")).collect();
        [(*s).to_owned(), hex]
    })
    .collect()
}

async fn selected_column_names(db: &TestDatabase, spec: &TableSpec) -> Vec<String> {
    let conn = db.connection();
    let pool = conn.get_postgres_connection_pool();
    let described = pool
        .prepare(AssertSqlSafe(spec.sql.replace("$1", "$1::uuid")).into_sql_str())
        .await
        .unwrap_or_else(|e| panic!("{}: {e:#}", spec.name));
    described
        .columns()
        .iter()
        .map(|c| c.name().to_owned())
        .collect()
}

#[tokio::test]
async fn exec_export_keeps_job_but_omits_secret_columns_and_values() {
    const SECRET_COLUMNS: [&str; 15] = [
        "token_hash",
        "api_key",
        "key",
        "password",
        "signature",
        "payload",
        "private_key",
        "device_code_hash",
        "user_code",
        "secret",
        "access_token",
        "encrypted_refresh_token",
        "user_agent",
        "ip",
        "allowed_ips",
    ];
    let Seeded { db, evaluation, .. } =
        seed_evaluation_with_dependencies_clusters_and_secrets().await;
    let project = db.rows::<project::Entity>().await[0].id;
    let forbidden = seed_credential_rows(&db).await;

    for spec in all_specs() {
        assert!(
            spec.columns.iter().all(|c| !SECRET_COLUMNS.contains(c)),
            "{}",
            spec.name
        );
        for column in selected_column_names(&db, spec).await {
            assert!(
                !SECRET_COLUMNS.contains(&column.as_str()),
                "{} selects {column}",
                spec.name
            );
        }
        let scope = if instance_tables().iter().any(|s| s.name == spec.name) {
            Uuid::from(project)
        } else {
            Uuid::from(evaluation)
        };
        for row in exported(&db, spec.name, scope).await {
            for cell in row.into_iter().flatten() {
                assert!(
                    !forbidden.iter().any(|secret| cell.contains(secret)),
                    "{} exports a secret: {cell}",
                    spec.name
                );
            }
        }
    }

    let spec = exported_column(&db, "exec_job", "spec", evaluation).await;
    assert_eq!(spec.len(), 1);
    assert!(
        spec.iter()
            .all(|s| s.contains("kept") && !s.contains("env"))
    );
    assert_eq!(
        exported_column(&db, "exec_job_phase", "payload_bytes", evaluation).await,
        ids(["phase-payload-secret".len()])
    );
    db.finish().await;
}

#[tokio::test]
async fn worker_samples_connections_and_derivation_metrics_are_exported_only_for_evaluation_workers_in_open_time_window()
 {
    let evaluation = EvaluationId::now_v7();
    let a = drv('a', "a");
    let db = shape()
        .evaluation(evaluation)
        .derivation(&a)
        .job()
        .derivation_metric(|m| m.created_at = hours_ago(72))
        .derivation_metric(|m| m.created_at = hours_ago(36))
        .derivation_metric(|m| m.created_at = hours_ago(12))
        .eval_assignment()
        .by_worker("w1")
        .into_database()
        .await;
    let conn = db.connection();
    let metrics = db
        .rows::<gradient_entity::derivation_metric::Entity>()
        .await;
    let metric_at = |hours: i64| {
        let at = hours_ago(hours);
        metrics
            .iter()
            .find(|m| (m.created_at - at).num_minutes().abs() < 5)
            .expect("metric")
            .id
    };

    let mut sample_ids = Vec::new();
    for (worker, hours) in [("w1", 72), ("w1", 36), ("w1", 12), ("w2", 36)] {
        let id = WorkerSampleId::now_v7();
        sample_ids.push((worker, hours, id));
        worker_sample::Model {
            id,
            worker_id: worker.into(),
            at: hours_ago(hours),
            ..Default::default()
        }
        .into_active_model()
        .insert(&conn)
        .await
        .expect("sample");
    }
    let mut connection_ids = Vec::new();
    for (worker, from, to) in [
        ("w1", 72, Some(60)),
        ("w1", 60, None),
        ("w1", 20, Some(10)),
        ("w2", 40, None),
    ] {
        let id = WorkerConnectionId::now_v7();
        connection_ids.push((worker, from, to, id));
        worker_connection::Model {
            id,
            worker_id: worker.into(),
            connected_at: hours_ago(from),
            disconnected_at: to.map(hours_ago),
            ..Default::default()
        }
        .into_active_model()
        .insert(&conn)
        .await
        .expect("connection");
    }
    let sample = |hours: i64| {
        sample_ids
            .iter()
            .find(|(w, h, _)| *w == "w1" && *h == hours)
            .expect("sample")
            .2
    };
    let connection = |from: i64| {
        connection_ids
            .iter()
            .find(|(w, f, _, _)| *w == "w1" && *f == from)
            .expect("connection")
            .3
    };

    db.execute(&format!(
        "UPDATE evaluation SET created_at = '{}', updated_at = '{}', finished_at = NULL WHERE id = '{evaluation}'",
        hours_ago(48),
        hours_ago(24)
    ))
    .await;
    assert_eq!(
        exported_column(&db, "derivation_metric", "id", evaluation).await,
        ids([metric_at(36), metric_at(12)])
    );
    assert_eq!(
        exported_column(&db, "worker_sample", "id", evaluation).await,
        ids([sample(36), sample(12)])
    );
    assert_eq!(
        exported_column(&db, "worker_connection", "id", evaluation).await,
        ids([connection(60), connection(20)])
    );

    db.execute(&format!(
        "UPDATE evaluation SET finished_at = '{}' WHERE id = '{evaluation}'",
        hours_ago(24)
    ))
    .await;
    assert_eq!(
        exported_column(&db, "derivation_metric", "id", evaluation).await,
        ids([metric_at(36)])
    );
    assert_eq!(
        exported_column(&db, "worker_sample", "id", evaluation).await,
        ids([sample(36)])
    );
    assert_eq!(
        exported_column(&db, "worker_connection", "id", evaluation).await,
        ids([connection(60)])
    );
    db.finish().await;
}

#[test]
fn test_every_table_spec_ddl_accepts_row_with_all_declared_columns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = gradient_report::open_report(&dir.path().join("r.db")).expect("open");
    for spec in all_specs() {
        gradient_report::create_table(&conn, spec).expect(spec.name);
        let row: Row = spec.columns.iter().map(|_| Some("1".to_owned())).collect();
        gradient_report::write_rows(&conn, spec, &[row]).expect(spec.name);
    }
}
