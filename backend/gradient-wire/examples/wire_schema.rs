/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::Path;

use gradient_wire::PROTO_VERSIONS;
use gradient_wire::schema::shape;

fn main() -> std::io::Result<()> {
    let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/schema"));
    std::fs::create_dir_all(dir)?;
    for version in PROTO_VERSIONS {
        let path = dir.join(format!("v{version}.txt"));
        if !path.exists() {
            std::fs::write(&path, shape(version))?;
            println!("wrote schema/v{version}.txt");
        }
    }

    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        let version = name
            .strip_prefix('v')
            .and_then(|rest| rest.strip_suffix(".txt"))
            .and_then(|number| number.parse::<u16>().ok());
        if version.is_some_and(|v| v < *PROTO_VERSIONS.start()) {
            std::fs::remove_file(dir.join(&name))?;
            println!("deleted schema/{name}");
        }
    }

    Ok(())
}
