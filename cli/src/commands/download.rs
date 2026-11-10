/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::attr_spec;
use crate::error::{CliError, FileAction};
use crate::input::{
    client_from_config, evaluation_uuid_from_reference, flush_stdout, selected_task,
};
use crate::output::{ExitKind, Output};
use connector::ConnectorError;
use connector::api::{ArtefactTree, ProductArtefact};
use connector::task_evaluation_build_reference_parser::TaskOrEvaluationReference;
use futures::{Stream, StreamExt, pin_mut};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

pub async fn handle_download(
    flake_ref: Option<String>,
    evaluation: Option<TaskOrEvaluationReference>,
    products: Option<String>,
    out_dir_arg: Option<String>,
    out: Output,
) {
    if out.is_json() && flake_ref.is_none() && products.is_none() {
        out.fail(CliError::MissingArgument("flake ref or --products"));
    }

    let client = client_from_config(out);

    let eval_id = match evaluation {
        Some(reference) => evaluation_uuid_from_reference(&client, &reference, out).await,
        None => pick_latest_evaluation(&client, out).await,
    };

    out.human(format!(
        "Fetching artefact tree for evaluation {}...",
        eval_id
    ));

    let tree = match client.evals().artefacts(&eval_id).await {
        Ok(t) => t,
        Err(e) => out.fail(e),
    };

    let flat = flatten_products(&tree);
    if flat.is_empty() {
        out.ok(&Vec::<String>::new());
        out.human("No artefacts in this evaluation.");
        return;
    }

    let selection = if let Some(spec) = &flake_ref {
        let attrs = attr_spec::parse(spec).unwrap_or_else(|e| out.err(ExitKind::Usage, e));
        select_by_attrs(&flat, &attrs).unwrap_or_else(|e| out.err(ExitKind::Api, e))
    } else if let Some(spec) = &products {
        parse_selection_spec(spec, flat.len()).unwrap_or_else(|e| out.err(ExitKind::Usage, e))
    } else if out.is_json() {
        out.fail(CliError::MissingArgument("flake ref or --products"))
    } else {
        interactive_select(&flat, out)
    };

    if selection.is_empty() {
        out.ok(&Vec::<String>::new());
        out.human("No products selected.");
        return;
    }

    let out_dir = out_dir_arg
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("read current directory"));

    if !out_dir.exists() {
        std::fs::create_dir_all(&out_dir)
            .unwrap_or_else(|e| out.fail(CliError::file(FileAction::Create, &out_dir, e)));
    }

    let destinations =
        build_product_download_destination_path_with_collision_subdirectory(&flat, &selection);
    let mut downloaded: Vec<String> = Vec::new();

    for (idx, relative_dest) in selection.into_iter().zip(destinations) {
        let p = &flat[idx];
        let display_name = product_filename(p.product);
        out.human(format!("Downloading {}...", display_name));
        let chunks = match client
            .builds()
            .download_file(&p.build_id, &display_name)
            .await
        {
            Ok(chunks) => chunks,
            Err(e) => out.err(
                ExitKind::from(&e),
                format!("Failed to download {}: {}", display_name, e),
            ),
        };
        let dest = out_dir.join(relative_dest);
        if let Some(parent) = dest.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|e| out.fail(CliError::file(FileAction::Create, parent, e)));
        }
        write_downloaded_byte_chunks_to_file(chunks, &dest)
            .await
            .unwrap_or_else(|e| out.fail(e));
        out.human(format!("  wrote {}", dest.display()));
        downloaded.push(dest.display().to_string());
    }

    out.ok(&downloaded);
}

struct FlatProduct<'a> {
    attr: &'a str,
    output_name: &'a str,
    build_id: String,
    product: &'a ProductArtefact,
}

fn flatten_products(tree: &ArtefactTree) -> Vec<FlatProduct<'_>> {
    let mut out = Vec::new();
    for ep in &tree.entry_points {
        for o in &ep.outputs {
            for p in &o.products {
                out.push(FlatProduct {
                    attr: &ep.attr,
                    output_name: &o.name,
                    build_id: ep.build_id.to_string(),
                    product: p,
                });
            }
        }
    }
    out
}

pub(crate) async fn write_downloaded_byte_chunks_to_file(
    chunks: impl Stream<Item = Result<impl AsRef<[u8]>, ConnectorError>>,
    dest: &Path,
) -> Result<(), CliError> {
    let mut file =
        std::fs::File::create(dest).map_err(|e| CliError::file(FileAction::Write, dest, e))?;
    pin_mut!(chunks);
    let written = async {
        while let Some(chunk) = chunks.next().await {
            file.write_all(chunk?.as_ref())
                .map_err(|e| CliError::file(FileAction::Write, dest, e))?;
        }
        Ok(())
    }
    .await;
    if written.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    written
}

pub(crate) fn product_filename(p: &ProductArtefact) -> String {
    Path::new(&p.path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| p.name.clone())
}

fn build_product_download_destination_path_with_collision_subdirectory(
    flat: &[FlatProduct<'_>],
    selection: &[usize],
) -> Vec<PathBuf> {
    let names: Vec<PathBuf> = selection
        .iter()
        .map(|&idx| safe_relative_name(&product_filename(flat[idx].product)))
        .collect();
    names
        .iter()
        .zip(selection)
        .map(|(name, &idx)| {
            if names.iter().filter(|other| *other == name).count() > 1 {
                safe_relative_name(flat[idx].attr).join(name)
            } else {
                name.clone()
            }
        })
        .collect()
}

pub(crate) fn safe_relative_name(name: &str) -> PathBuf {
    let buf: PathBuf = Path::new(name)
        .components()
        .filter_map(|comp| match comp {
            std::path::Component::Normal(c) => Some(c),
            _ => None,
        })
        .collect();
    if buf.as_os_str().is_empty() {
        PathBuf::from("download")
    } else {
        buf
    }
}

fn interactive_select(flat: &[FlatProduct<'_>], out: Output) -> Vec<usize> {
    println!("\nAvailable artefacts:");
    for (i, p) in flat.iter().enumerate() {
        println!(
            "  {:>3}. {} / {} / {}{}",
            i + 1,
            p.attr,
            p.output_name,
            p.product.path,
            p.product
                .size
                .map(|s| format!(" ({})", human_size(s)))
                .unwrap_or_default(),
        );
    }
    print!(
        "\nSelect products (comma-separated 1-{}, ranges like 1-3, or 'all'): ",
        flat.len()
    );
    flush_stdout(out);

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .unwrap_or_else(|e| out.fail(CliError::ReadInput("selection".into(), e)));

    parse_selection_spec(input.trim(), flat.len()).unwrap_or_else(|e| out.err(ExitKind::Usage, e))
}

fn parse_selection_spec(spec: &str, total: usize) -> Result<Vec<usize>, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Ok(Vec::new());
    }
    if spec.eq_ignore_ascii_case("all") {
        return Ok((0..total).collect());
    }
    let mut out = std::collections::BTreeSet::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((lo, hi)) = part.split_once('-') {
            let lo: usize = lo
                .trim()
                .parse()
                .map_err(|_| format!("invalid index '{}'", lo))?;
            let hi: usize = hi
                .trim()
                .parse()
                .map_err(|_| format!("invalid index '{}'", hi))?;
            if lo == 0 || hi == 0 || lo > total || hi > total || lo > hi {
                return Err(format!("range '{}' out of bounds 1..={}", part, total));
            }
            for i in lo..=hi {
                out.insert(i - 1);
            }
        } else {
            let n: usize = part
                .parse()
                .map_err(|_| format!("invalid index '{}'", part))?;
            if n == 0 || n > total {
                return Err(format!("index '{}' out of bounds 1..={}", n, total));
            }
            out.insert(n - 1);
        }
    }
    Ok(out.into_iter().collect())
}

fn select_by_attrs(flat: &[FlatProduct<'_>], attrs: &[String]) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    let mut unmatched = Vec::new();
    for want in attrs {
        let mut found = false;
        for (i, p) in flat.iter().enumerate() {
            if p.attr == want {
                if !out.contains(&i) {
                    out.push(i);
                }
                found = true;
            }
        }
        if !found {
            unmatched.push(want.clone());
        }
    }
    if !unmatched.is_empty() {
        let mut available: Vec<&str> = flat.iter().map(|p| p.attr).collect();
        available.sort();
        available.dedup();
        return Err(format!(
            "attr not found in evaluation: {}; available: {}",
            unmatched.join(", "),
            available.join(", "),
        ));
    }
    Ok(out)
}

pub(crate) fn human_size(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut i = 0;
    while value >= 1024.0 && i < UNITS.len() - 1 {
        value /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", value, UNITS[i])
    }
}

async fn pick_latest_evaluation(client: &connector::Client, out: Output) -> String {
    let (project, task) = selected_task(out);
    let evaluations = match client.tasks().evaluations(&project, &task).await {
        Ok(evals) => evals,
        Err(e) => out.fail(e),
    };
    let latest = evaluations.into_iter().next().unwrap_or_else(|| {
        out.err(
            ExitKind::Api,
            format!("No evaluations found for {}/{}.", project, task),
        );
    });
    out.human(format!(
        "Using latest evaluation {} for task {}/{}.",
        latest.id, project, task
    ));
    latest.id.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use connector::api::{ArtefactTree, EntryPointArtefacts, OutputArtefacts, ProductArtefact};

    const EVALUATION: &str = "00000000-0000-0000-0000-0000000000e1";
    const BUILD_A: &str = "00000000-0000-0000-0000-0000000000b1";
    const BUILD_B: &str = "00000000-0000-0000-0000-0000000000b2";
    const PRODUCT_A: &str = "00000000-0000-0000-0000-0000000000a1";
    const PRODUCT_B: &str = "00000000-0000-0000-0000-0000000000a2";

    fn make_tree() -> ArtefactTree {
        ArtefactTree {
            evaluation: EVALUATION.parse().unwrap(),
            created_at: "2026-01-01T00:00:00Z".into(),
            entry_points: vec![
                EntryPointArtefacts {
                    attr: "packages.x86_64-linux.my-app".into(),
                    derivation: "/nix/store/a.drv".into(),
                    build_id: BUILD_A.parse().unwrap(),
                    outputs: vec![OutputArtefacts {
                        name: "out".into(),
                        store_path: "/nix/store/a".into(),
                        products: vec![ProductArtefact {
                            id: PRODUCT_A.parse().unwrap(),
                            type_: "file".into(),
                            subtype: "".into(),
                            name: "default".into(),
                            path: "/p/my-app-1.0.tar.gz".into(),
                            size: Some(1024),
                        }],
                    }],
                },
                EntryPointArtefacts {
                    attr: "packages.x86_64-linux.cli".into(),
                    derivation: "/nix/store/b.drv".into(),
                    build_id: BUILD_B.parse().unwrap(),
                    outputs: vec![OutputArtefacts {
                        name: "out".into(),
                        store_path: "/nix/store/b".into(),
                        products: vec![ProductArtefact {
                            id: PRODUCT_B.parse().unwrap(),
                            type_: "file".into(),
                            subtype: "".into(),
                            name: "default".into(),
                            path: "/p/cli-2.tgz".into(),
                            size: None,
                        }],
                    }],
                },
            ],
        }
    }

    #[test]
    fn select_single_attr() {
        let tree = make_tree();
        let flat = flatten_products(&tree);
        let sel = select_by_attrs(&flat, &["packages.x86_64-linux.my-app".to_string()]).unwrap();
        assert_eq!(sel, vec![0]);
    }

    #[test]
    fn select_multiple_attrs() {
        let tree = make_tree();
        let flat = flatten_products(&tree);
        let sel = select_by_attrs(
            &flat,
            &[
                "packages.x86_64-linux.my-app".to_string(),
                "packages.x86_64-linux.cli".to_string(),
            ],
        )
        .unwrap();
        assert_eq!(sel, vec![0, 1]);
    }

    #[test]
    fn select_no_match_lists_available() {
        let tree = make_tree();
        let flat = flatten_products(&tree);
        let err = select_by_attrs(&flat, &["nope".to_string()]).unwrap_err();
        assert!(err.contains("packages.x86_64-linux.my-app"));
        assert!(err.contains("packages.x86_64-linux.cli"));
    }

    #[test]
    fn safe_relative_name_stays_inside_the_target_directory() {
        assert_eq!(safe_relative_name("a.tgz"), PathBuf::from("a.tgz"));
        assert_eq!(
            safe_relative_name("/etc/passwd"),
            PathBuf::from("etc/passwd")
        );
        assert_eq!(
            safe_relative_name("../../.bashrc"),
            PathBuf::from(".bashrc")
        );
        assert_eq!(safe_relative_name("a/./../b"), PathBuf::from("a/b"));
        assert_eq!(safe_relative_name(".."), PathBuf::from("download"));
    }

    #[test]
    fn products_with_the_same_file_name_go_to_their_entry_point_directory() {
        let mut tree = make_tree();
        tree.entry_points[1].outputs[0].products[0].path = "/p/my-app-1.0.tar.gz".into();
        let flat = flatten_products(&tree);

        assert_eq!(
            build_product_download_destination_path_with_collision_subdirectory(&flat, &[0, 1]),
            vec![
                PathBuf::from("packages.x86_64-linux.my-app/my-app-1.0.tar.gz"),
                PathBuf::from("packages.x86_64-linux.cli/my-app-1.0.tar.gz"),
            ]
        );
        assert_eq!(
            build_product_download_destination_path_with_collision_subdirectory(&flat, &[1]),
            vec![PathBuf::from("my-app-1.0.tar.gz")]
        );
    }
}
