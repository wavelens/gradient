/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation_message;
use gradient_entity::ids::EvaluationId;
use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping};
use gradient_fixtures::shape::shape;
use gradient_wire::types::{BuildOutput, BuildProduct, DerivationOutput};

use super::{A, A_OUT, B_OUT, Case, drv, nar, store_path, with_attr};

fn content_addressed(path: &str) -> gradient_wire::types::DiscoveredDerivation {
    let mut derivation = with_attr(drv(path, &[]), "pkg.a");
    derivation.outputs.push(DerivationOutput {
        name: "out".into(),
        path: String::new(),
    });
    derivation
}

fn realised(hash: &str, package: &str) -> BuildOutput {
    BuildOutput {
        name: "out".into(),
        store_path: store_path(hash, package),
        hash: hash.into(),
        nar_size: None,
        nar_hash: None,
        products: Vec::new(),
    }
}

#[tokio::test]
async fn r01_a_build_records_the_realised_path_of_a_content_addressed_output() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    case.walk(case.tasked(e1, vec![content_addressed(A)]).await)
        .await;

    let unrealised = case.output_of(A, "out").await;
    assert_eq!(
        (
            unrealised.hash,
            unrealised.package,
            unrealised.path_known_after_build
        ),
        (None, None, true),
        "after 1: output(A,out)"
    );

    case.built_outputs(A, vec![realised(A_OUT, "a")], false)
        .await;

    let output = case.output_of(A, "out").await;
    assert_eq!(
        (output.hash.as_deref(), output.package.as_deref()),
        (Some(A_OUT), Some("a")),
        "after 2: output(A,out)"
    );
    case.finish().await;
}

#[tokio::test]
async fn r02_completed_build_after_upload_of_content_addressed_output_with_path_known_only_after_build()
 {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    case.walk(case.tasked(e1, vec![content_addressed(A)]).await)
        .await;
    case.build_started(A).await;

    case.built_outputs(A, vec![realised(A_OUT, "a")], false)
        .await;
    case.commit_nar(nar(A_OUT, "a")).await;
    case.build_completed(A).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.fetchable),
        (BuildStatus::Completed, true),
        "after 3: sb(A)"
    );
    case.finish().await;
}

#[tokio::test]
async fn build_output_report_for_old_dispatched_job_after_requeue_is_ignored() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    case.walk(case.tasked(e1, vec![content_addressed(A)]).await)
        .await;

    let old_dispatch = case
        .insert_open_dispatched_job_for_shared_build_without_claim_gate(A)
        .await;
    case.close_assignment(A).await;
    let new_dispatch = case
        .insert_open_dispatched_job_for_shared_build_without_claim_gate(A)
        .await;

    case.send_build_output_for_build_path_naming_dispatched_job_id(
        A,
        old_dispatch,
        vec![realised(B_OUT, "a")],
        false,
    )
    .await;
    let output = case.output_of(A, "out").await;
    assert_eq!(
        (output.hash, output.package),
        (None, None),
        "after 1: the report of the old dispatch is ignored"
    );

    case.send_build_output_for_build_path_naming_dispatched_job_id(
        A,
        new_dispatch,
        vec![realised(A_OUT, "a")],
        false,
    )
    .await;
    let output = case.output_of(A, "out").await;
    assert_eq!(
        (output.hash.as_deref(), output.package.as_deref()),
        (Some(A_OUT), Some("a")),
        "after 2: the report of the open dispatch is recorded"
    );
    case.finish().await;
}

#[tokio::test]
async fn missing_hydra_build_product_warning_names_original_path_not_stored_path_with_rotated_mapping()
 {
    let e1 = EvaluationId::now_v7();
    let case = Case::start_with_mapping(shape().evaluation(e1), Arc::new(RotatedHashMapping)).await;
    case.walk(case.tasked(e1, vec![content_addressed(A)]).await)
        .await;

    let product = BuildProduct {
        file_type: "file".into(),
        subtype: "doc".into(),
        name: "file".into(),
        path: format!("/nix/store/{EXAMPLE_STORED_HASH}-out/file"),
        size: None,
    };
    let output = BuildOutput {
        products: vec![product],
        ..realised(A_OUT, "a")
    };
    case.built_outputs(A, vec![output], false).await;

    let messages: Vec<String> = case
        .db
        .rows::<evaluation_message::Entity>()
        .await
        .into_iter()
        .filter(|m| m.evaluation == e1)
        .map(|m| m.message)
        .collect();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains(EXAMPLE_ORIGINAL_HASH), "{messages:?}");
    assert!(!messages[0].contains(EXAMPLE_STORED_HASH), "{messages:?}");
    case.finish().await;
}
