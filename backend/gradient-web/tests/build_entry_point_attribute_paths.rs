/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::EvaluationId;
use gradient_fixtures::shape::shape;
use gradient_test_support::state::TestState;
use serde_json::{Value, json};

const APP: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1-app.drv";
const LIBRARY: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa2-library.drv";
const APP_ATTRIBUTE: &str = "packages.x86_64-linux.app";

#[tokio::test]
async fn an_entry_point_build_names_its_attribute_path_and_a_dependency_names_none() {
    let evaluation = EvaluationId::now_v7();
    let db = shape()
        .evaluation(evaluation)
        .evaluation_status(EvaluationStatus::Building)
        .derivation(APP)
        .depends_on(LIBRARY)
        .job()
        .entry_point(APP_ATTRIBUTE)
        .derivation(LIBRARY)
        .job()
        .into_database()
        .await;
    db.execute("UPDATE project SET public = true").await;
    let app = db.job(evaluation, APP).await.expect("the app has a build");
    let library = db
        .job(evaluation, LIBRARY)
        .await
        .expect("the library has a build");
    let server = TestState::new().web_db(db.connection()).server();

    let list = server
        .get(&format!("/api/v1/evals/{evaluation}/builds"))
        .await;
    list.assert_status_ok();
    let builds = list.json::<Value>()["message"]["builds"].clone();
    let listed = |id: String| {
        builds
            .as_array()
            .expect("the builds are a list")
            .iter()
            .find(|build| build["id"] == json!(id))
            .map(|build| build["entry_points"].clone())
            .expect("the build is listed")
    };
    assert_eq!(listed(app.id.to_string()), json!([APP_ATTRIBUTE]));
    assert_eq!(listed(library.id.to_string()), json!([]));

    for (build, expected) in [(app.id, json!([APP_ATTRIBUTE])), (library.id, json!([]))] {
        let single = server.get(&format!("/api/v1/builds/{build}")).await;
        single.assert_status_ok();
        assert_eq!(single.json::<Value>()["message"]["entry_points"], expected);
    }
    db.finish().await;
}
