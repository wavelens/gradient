/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;
use std::path::Path;

use gradient_wire::PROTO_VERSIONS;
use gradient_wire::schema::shape;

fn schema_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/schema"))
}

#[test]
fn a_schema_file_exists_for_exactly_the_supported_versions() {
    let files: BTreeSet<String> = std::fs::read_dir(schema_dir())
        .expect("schema dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    let expected: BTreeSet<String> = PROTO_VERSIONS.map(|v| format!("v{v}.txt")).collect();
    assert_eq!(
        files, expected,
        "run `cargo run --example wire_schema` and commit the result"
    );
}

#[test]
fn every_supported_version_still_has_its_released_shape() {
    for version in PROTO_VERSIONS {
        let stored = std::fs::read_to_string(schema_dir().join(format!("v{version}.txt")))
            .expect("schema file");
        assert_eq!(
            shape(version),
            stored,
            "the wire shape of protocol {version} changed: raise `#[proto(oldest = N)]` or undo the change"
        );
    }
}
