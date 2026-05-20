/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::attr_spec;
use crate::config::*;
use crate::input::client_from_config;
use crate::output::{ExitKind, Output, to_exit_kind};
use connector::evals::{ArtefactTree, ProductArtefact};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::exit;

pub async fn handle_download(
    flake_ref: Option<String>,
    evaluation: Option<String>,
    project: Option<String>,
    products: Option<String>,
    out_dir_arg: Option<String>,
    out: Output,
) {
    let client = client_from_config(out);

    let eval_id = match evaluation {
        Some(id) => id,
        None => pick_latest_evaluation(&client, project.as_deref(), out).await,
    };

    out.human(format!("Fetching artefact tree for evaluation {}...", eval_id));

    let tree = match client.evals().artefacts(&eval_id).await {
        Ok(t) => t,
        Err(e) => out.err(to_exit_kind(&e), e),
    };

    let flat = flatten_products(&tree);
    if flat.is_empty() {
        out.human("No artefacts in this evaluation.");
        return;
    }

    let selection = if let Some(spec) = &flake_ref {
        let attrs = attr_spec::parse(spec).unwrap_or_else(|e| out.err(ExitKind::Usage, e));
        select_by_attrs(&flat, &attrs).unwrap_or_else(|e| out.err(ExitKind::Api, e))
    } else if let Some(spec) = &products {
        parse_selection_spec(spec, flat.len()).unwrap_or_else(|e| out.err(ExitKind::Usage, e))
    } else if out.is_json() {
        out.err(ExitKind::Usage, "missing argument: flake ref or --products")
    } else {
        interactive_select(&flat)
    };

    if selection.is_empty() {
        out.human("No products selected.");
        return;
    }

    let out_dir = out_dir_arg
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("read current directory"));

    if !out_dir.exists() {
        std::fs::create_dir_all(&out_dir).unwrap_or_else(|e| {
            out.err(ExitKind::Api, format!("Failed to create {}: {}", out_dir.display(), e));
        });
    }

    let mut downloaded: Vec<String> = Vec::new();

    for idx in selection {
        let p = &flat[idx];
        let display_name = product_filename(p.product);
        out.human(format!("Downloading {}...", display_name));
        let bytes = match client.builds().download_file(&p.build_id, &display_name).await {
            Ok(b) => b,
            Err(e) => out.err(to_exit_kind(&e), format!("Failed to download {}: {}", display_name, e)),
        };
        let dest = out_dir.join(safe_relative_name(&display_name));
        if let Some(parent) = dest.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).unwrap_or_else(|e| {
                out.err(ExitKind::Api, format!("Failed to create {}: {}", parent.display(), e));
            });
        }
        std::fs::write(&dest, bytes).unwrap_or_else(|e| {
            out.err(ExitKind::Api, format!("Failed to write {}: {}", dest.display(), e));
        });
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
                    build_id: ep.build_id.clone(),
                    product: p,
                });
            }
        }
    }
    out
}

fn product_filename(p: &ProductArtefact) -> String {
    Path::new(&p.path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| p.name.clone())
}

fn safe_relative_name(name: &str) -> PathBuf {
    let stripped = name.trim_start_matches('/');
    let mut buf = PathBuf::new();
    for comp in Path::new(stripped).components() {
        use std::path::Component;
        match comp {
            Component::Normal(c) => buf.push(c),
            _ => buf.push(comp.as_os_str()),
        }
    }
    if buf.as_os_str().is_empty() { PathBuf::from("download") } else { buf }
}

fn interactive_select(flat: &[FlatProduct<'_>]) -> Vec<usize> {
    println!("\nAvailable artefacts:");
    for (i, p) in flat.iter().enumerate() {
        println!(
            "  {:>3}. {} / {} / {}{}",
            i + 1,
            p.attr,
            p.output_name,
            p.product.path,
            p.product.size.map(|s| format!(" ({})", human_size(s))).unwrap_or_default(),
        );
    }
    print!("\nSelect products (comma-separated 1-{}, ranges like 1-3, or 'all'): ", flat.len());
    io::stdout().flush().unwrap();
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    parse_selection_spec(input.trim(), flat.len()).unwrap_or_else(|e| {
        eprintln!("{}", e);
        exit(1);
    })
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
            let lo: usize = lo.trim().parse().map_err(|_| format!("invalid index '{}'", lo))?;
            let hi: usize = hi.trim().parse().map_err(|_| format!("invalid index '{}'", hi))?;
            if lo == 0 || hi == 0 || lo > total || hi > total || lo > hi {
                return Err(format!("range '{}' out of bounds 1..={}", part, total));
            }
            for i in lo..=hi {
                out.insert(i - 1);
            }
        } else {
            let n: usize = part.parse().map_err(|_| format!("invalid index '{}'", part))?;
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

fn human_size(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut i = 0;
    while value >= 1024.0 && i < UNITS.len() - 1 {
        value /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{} {}", bytes, UNITS[0]) } else { format!("{:.1} {}", value, UNITS[i]) }
}

async fn pick_latest_evaluation(
    client: &connector::Client,
    project: Option<&str>,
    out: Output,
) -> String {
    let (organization, project) = resolve_project(project, out);
    let evaluations = match client.projects().evaluations(&organization, &project).await {
        Ok(evals) => evals,
        Err(e) => out.err(to_exit_kind(&e), e),
    };
    let latest = evaluations.into_iter().next().unwrap_or_else(|| {
        out.err(ExitKind::Api, format!("No evaluations found for {}/{}.", organization, project));
    });
    out.human(format!(
        "Using latest evaluation {} for project {}/{}.",
        latest.id, organization, project
    ));
    latest.id
}

fn resolve_project(arg: Option<&str>, out: Output) -> (String, String) {
    if let Some(spec) = arg {
        if let Some((org, proj)) = spec.split_once('/') {
            return (org.to_string(), proj.to_string());
        }
        let organization = set_get_value(ConfigKey::SelectedOrganization, None, true)
            .unwrap_or_else(|| {
                out.err(
                    ExitKind::Usage,
                    format!("--project '{}' has no org prefix and no organization is selected.", spec),
                );
            });
        return (organization, spec.to_string());
    }

    if let Some(selected) = set_get_value(ConfigKey::SelectedProject, None, true)
        && let Some((org, proj)) = selected.split_once('/')
    {
        return (org.to_string(), proj.to_string());
    }

    out.err(
        ExitKind::Usage,
        "No project selected. Pass --project <name> or run 'gradient project select <name>' first.",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use connector::evals::{ArtefactTree, EntryPointArtefacts, OutputArtefacts, ProductArtefact};

    fn make_tree() -> ArtefactTree {
        ArtefactTree {
            evaluation: "eval-1".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            entry_points: vec![
                EntryPointArtefacts {
                    attr: "packages.x86_64-linux.my-app".into(),
                    derivation: "/nix/store/a.drv".into(),
                    build_id: "b1".into(),
                    outputs: vec![OutputArtefacts {
                        name: "out".into(),
                        store_path: "/nix/store/a".into(),
                        products: vec![ProductArtefact {
                            id: "p1".into(),
                            file_type: "file".into(),
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
                    build_id: "b2".into(),
                    outputs: vec![OutputArtefacts {
                        name: "out".into(),
                        store_path: "/nix/store/b".into(),
                        products: vec![ProductArtefact {
                            id: "p2".into(),
                            file_type: "file".into(),
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
        let sel = select_by_attrs(&flat, &[
            "packages.x86_64-linux.my-app".to_string(),
            "packages.x86_64-linux.cli".to_string(),
        ]).unwrap();
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
}
