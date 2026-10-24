/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::ids::EvaluationId;
use gradient_fixtures::shape::shape;
use gradient_wire::types::{BuildOutput, DerivationOutput};

use super::{A, A_OUT, Case, drv, store_path, with_attr};

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
