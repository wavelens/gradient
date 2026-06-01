/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::client_from_config;
use crate::output::{ExitKind, Output, to_exit_kind};
use clap::Subcommand;
use connector::caches::NarListQuery;
use std::io::{BufRead, Write};

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// List NARs in a cache
    List {
        cache: String,
        #[arg(long)]
        hash: Option<String>,
        #[arg(long)]
        package: Option<String>,
        #[arg(long, value_parser = ["created_at", "nar_size", "last_fetched_at"])]
        sort: Option<String>,
        #[arg(long, value_parser = ["asc", "desc"])]
        order: Option<String>,
        #[arg(long)]
        page: Option<u32>,
        #[arg(long = "per-page")]
        per_page: Option<u32>,
        #[arg(short = 'i', long)]
        interactive: bool,
    },
    /// Show a NAR's full metadata
    Show { cache: String, hash: String },
    /// Delete a NAR from a cache
    Delete {
        cache: String,
        hash: String,
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Aggregate stats for a cache's NARs
    Stats { cache: String },
}

pub async fn handle(cmd: Commands, out: Output) {
    match cmd {
        Commands::List {
            cache,
            hash,
            package,
            sort,
            order,
            page,
            per_page,
            interactive,
        } => {
            let client = client_from_config(out);
            let q = NarListQuery {
                hash,
                package,
                sort,
                order,
                page,
                per_page,
            };
            match client.caches().nars_list(&cache, q).await {
                Ok(res) => {
                    if interactive && !out.is_json() {
                        crate::tui::run(crate::tui::nar_browser::NarBrowser::new(res.items))
                            .unwrap_or_else(|e| out.err(ExitKind::Api, format!("tui error: {e}")));
                        return;
                    }
                    out.ok(&res);
                    if res.items.is_empty() {
                        out.human("No NARs match.");
                    } else {
                        for item in &res.items {
                            let lf = item.last_fetched_at.as_deref().unwrap_or("never");
                            let short = &item.hash[..item.hash.len().min(16)];
                            out.human(format!(
                                "{}  {}  size={}  last_fetched={}",
                                short,
                                item.package,
                                item.nar_size.unwrap_or(0),
                                lf,
                            ));
                        }
                        let pages = if res.per_page == 0 {
                            1
                        } else {
                            res.total.div_ceil(res.per_page)
                        };
                        out.human(format!("page {}/{} (total {})", res.page, pages, res.total));
                    }
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::Show { cache, hash } => {
            let client = client_from_config(out);
            match client.caches().nar_show(&cache, &hash).await {
                Ok(d) => {
                    out.ok(&d);
                    out.human(format!("hash:        {}", d.hash));
                    out.human(format!("store_path:  {}", d.store_path));
                    out.human(format!("package:     {}", d.package));
                    if let Some(s) = d.nar_size {
                        out.human(format!("nar_size:    {}", s));
                    }
                    if let Some(s) = d.file_size {
                        out.human(format!("file_size:   {}", s));
                    }
                    out.human(format!("signed:      {}", d.signed));
                    out.human(format!("fetch_count: {}", d.fetch_count));
                    if let Some(t) = d.last_fetched_at {
                        out.human(format!("last_fetched_at: {}", t));
                    }
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::Delete { cache, hash, yes } => {
            if !yes {
                if out.is_json() {
                    out.err(
                        ExitKind::Usage,
                        "Refusing to delete without --yes in --json mode",
                    );
                }
                let mut stderr = std::io::stderr();
                write!(stderr, "Delete NAR {hash} from cache '{cache}'? [y/N]: ").ok();
                stderr.flush().ok();
                let mut line = String::new();
                std::io::stdin().lock().read_line(&mut line).ok();
                if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    out.human("Cancelled.");
                    return;
                }
            }
            let client = client_from_config(out);
            match client.caches().nar_delete(&cache, &hash).await {
                Ok(()) => {
                    out.ok(&serde_json::json!({"deleted": true, "hash": hash}));
                    out.human("NAR deleted.");
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::Stats { cache } => {
            let client = client_from_config(out);
            match client.caches().nars_stats(&cache).await {
                Ok(s) => {
                    out.ok(&s);
                    out.human(format!("total_nars:        {}", s.total_nars));
                    out.human(format!("total_nar_size:    {}", s.total_nar_size));
                    out.human(format!("total_file_size:   {}", s.total_file_size));
                    if let Some(t) = s.last_uploaded_at {
                        out.human(format!("last_uploaded_at:  {}", t));
                    }
                    if let Some(t) = s.oldest_fetched_at {
                        out.human(format!("oldest_fetched_at: {}", t));
                    }
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }
    }
}
