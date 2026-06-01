/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::client_from_config;
use crate::narinfo::Narinfo;
use crate::output::{ExitKind, Output, to_exit_kind};
use connector::caches::NarinfoUpload;
use std::path::PathBuf;

#[derive(clap::Args, Debug)]
pub struct UploadArgs {
    /// Cache name to upload into.
    pub cache: String,
    /// Store paths to upload (requires the CLI built with the `nix` feature).
    pub paths: Vec<String>,
    /// Upload a pre-dumped NAR file without any local nix (needs --narinfo).
    #[arg(long)]
    pub nar_file: Option<PathBuf>,
    /// Narinfo metadata file describing --nar-file.
    #[arg(long)]
    pub narinfo: Option<PathBuf>,
    /// Upload the full runtime closure of each store path (nix feature only).
    #[arg(long)]
    pub full_closure: bool,
}

pub async fn handle(args: UploadArgs, out: Output) {
    if let Some(nar_file) = &args.nar_file {
        let Some(narinfo_path) = &args.narinfo else {
            out.err(ExitKind::Usage, "--nar-file requires --narinfo");
        };
        let narinfo_text = std::fs::read_to_string(narinfo_path)
            .unwrap_or_else(|e| out.err(ExitKind::Usage, format!("cannot read narinfo: {e}")));
        let ni = Narinfo::parse(&narinfo_text)
            .unwrap_or_else(|e| out.err(ExitKind::Usage, format!("bad narinfo: {e}")));
        let bytes = std::fs::read(nar_file)
            .unwrap_or_else(|e| out.err(ExitKind::Usage, format!("cannot read nar: {e}")));
        upload_one_owned(&args.cache, ni, bytes, out).await;
        return;
    }

    if args.paths.is_empty() {
        out.err(ExitKind::Usage, "provide store path(s) or --nar-file/--narinfo");
    }

    out.err(
        ExitKind::Usage,
        "store-path upload requires a CLI built with the `nix` feature; use --nar-file/--narinfo instead",
    );
}

pub(crate) async fn upload_one_owned(cache: &str, ni: Narinfo, bytes: Vec<u8>, out: Output) {
    let client = client_from_config(out);
    let store_path = ni.store_path.clone();
    let payload = NarinfoUpload {
        store_path: ni.store_path,
        file_hash: ni.file_hash,
        file_size: ni.file_size,
        nar_size: ni.nar_size,
        nar_hash: ni.nar_hash,
        references: ni.references,
        deriver: ni.deriver,
    };
    match client.caches().nar_upload(cache, payload, bytes).await {
        Ok(()) => {
            out.ok(&serde_json::json!({"uploaded": true, "store_path": store_path}));
            out.human(format!("Uploaded {store_path}"));
        }
        Err(e) => out.err(to_exit_kind(&e), e),
    }
}
