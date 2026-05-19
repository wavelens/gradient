/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::*;
use crate::input::*;
use connector::build_requests::{ArtefactTree, ProductArtefact};
use connector::{RequestConfig, build_requests, builds, projects};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::exit;

pub async fn handle_download(
    evaluation: Option<String>,
    project: Option<String>,
    products: Option<String>,
    out: Option<String>,
) {
    let config = get_request_config(load_config()).unwrap_or_else(|_| {
        eprintln!("Not configured. Use 'gradient config' to set server and auth token.");
        exit(1);
    });

    let eval_id = match evaluation {
        Some(id) => id,
        None => pick_latest_evaluation(&config, project.as_deref()).await,
    };

    println!("Fetching artefact tree for evaluation {}...", eval_id);

    let tree = match build_requests::get_eval_artefacts(config.clone(), eval_id).await {
        Ok(r) if r.error => {
            eprintln!("Server rejected request for artefact tree.");
            exit(1);
        }
        Ok(r) => r.message,
        Err(e) => {
            eprintln!("Failed to fetch artefact tree: {}", e);
            exit(1);
        }
    };

    let flat = flatten_products(&tree);
    if flat.is_empty() {
        println!("No artefacts in this evaluation.");
        return;
    }

    let selection = match products {
        Some(spec) => parse_selection_spec(&spec, flat.len()).unwrap_or_else(|e| {
            eprintln!("{}", e);
            exit(1);
        }),
        None => interactive_select(&flat),
    };

    if selection.is_empty() {
        println!("No products selected.");
        return;
    }

    let out_dir = out
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("read current directory"));

    if !out_dir.exists() {
        std::fs::create_dir_all(&out_dir).unwrap_or_else(|e| {
            eprintln!("Failed to create {}: {}", out_dir.display(), e);
            exit(1);
        });
    }

    for idx in selection {
        let p = &flat[idx];
        let display_name = product_filename(p.product);
        println!("Downloading {}...", display_name);
        let bytes = match builds::download_build_file(
            config.clone(),
            p.build_id.clone(),
            display_name.clone(),
        )
        .await
        {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Failed to download {}: {}", display_name, e);
                exit(1);
            }
        };
        let dest = out_dir.join(safe_relative_name(&display_name));
        if let Some(parent) = dest.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).unwrap_or_else(|e| {
                eprintln!("Failed to create {}: {}", parent.display(), e);
                exit(1);
            });
        }
        std::fs::write(&dest, bytes).unwrap_or_else(|e| {
            eprintln!("Failed to write {}: {}", dest.display(), e);
            exit(1);
        });
        println!("  wrote {}", dest.display());
    }
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
    if buf.as_os_str().is_empty() {
        PathBuf::from("download")
    } else {
        buf
    }
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

fn human_size(bytes: i64) -> String {
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

async fn pick_latest_evaluation(config: &RequestConfig, project: Option<&str>) -> String {
    let (organization, project) = resolve_project(project);
    let evaluations = match projects::get_project_evaluations(
        config.clone(),
        organization.clone(),
        project.clone(),
    )
    .await
    {
        Ok(r) if r.error => {
            eprintln!("Server rejected request for project evaluations.");
            exit(1);
        }
        Ok(r) => r.message,
        Err(e) => {
            eprintln!("Failed to list project evaluations: {}", e);
            exit(1);
        }
    };
    let latest = evaluations.into_iter().next().unwrap_or_else(|| {
        eprintln!("No evaluations found for {}/{}.", organization, project);
        exit(1);
    });
    println!(
        "Using latest evaluation {} for project {}/{}.",
        latest.id, organization, project
    );
    latest.id
}

fn resolve_project(arg: Option<&str>) -> (String, String) {
    if let Some(spec) = arg {
        if let Some((org, proj)) = spec.split_once('/') {
            return (org.to_string(), proj.to_string());
        }
        let organization = set_get_value(ConfigKey::SelectedOrganization, None, true)
            .unwrap_or_else(|| {
                eprintln!(
                    "--project '{}' has no org prefix and no organization is selected.",
                    spec
                );
                exit(1);
            });
        return (organization, spec.to_string());
    }

    if let Some(selected) = set_get_value(ConfigKey::SelectedProject, None, true)
        && let Some((org, proj)) = selected.split_once('/')
    {
        return (org.to_string(), proj.to_string());
    }

    eprintln!(
        "No project selected. Pass --project <name> or run 'gradient project select <name>' first."
    );
    exit(1);
}
