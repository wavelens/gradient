/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::completion;
use crate::input::client_from_config;
use crate::narinfo::Narinfo;
use crate::output::{ExitKind, Output, to_exit_kind};
use clap_complete::engine::ArgValueCompleter;
use connector::caches::NarinfoUpload;
use std::path::PathBuf;

#[derive(clap::Args, Debug)]
pub struct UploadArgs {
    /// Cache name to upload into.
    #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
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

    #[cfg(not(feature = "nix"))]
    out.err(
        ExitKind::Usage,
        "store-path upload requires a CLI built with the `nix` feature; use --nar-file/--narinfo instead",
    );

    #[cfg(feature = "nix")]
    crate::commands::cache_upload_nix::upload_paths(&args, out).await;
}

/// NAR slice size per chunked-upload request. Comfortably below the bundled
/// reverse proxy's 100 MiB body cap so each request always gets through.
const UPLOAD_CHUNK_SIZE: usize = 32 * 1024 * 1024;

/// The 32-char store hash, used as the server-side staging key. URL-safe by
/// construction (lowercase base32), unlike full store names which can carry
/// `+`/`?`/`=`.
fn store_hash_of(store_path: &str) -> &str {
    let base = store_path.rsplit('/').next().unwrap_or(store_path);
    base.split('-').next().unwrap_or(base)
}

pub(crate) async fn upload_one_owned(cache: &str, ni: Narinfo, bytes: Vec<u8>, out: Output) {
    let client = client_from_config(out);
    let store_path = ni.store_path.clone();
    let store_hash = store_hash_of(&store_path).to_string();
    let payload = NarinfoUpload {
        store_path: ni.store_path,
        file_hash: ni.file_hash,
        file_size: ni.file_size,
        nar_size: ni.nar_size,
        nar_hash: ni.nar_hash,
        references: ni.references,
        deriver: ni.deriver,
    };

    let total = bytes.len() as u64;
    let mut offset = 0u64;
    while offset < total {
        let end = (offset as usize + UPLOAD_CHUNK_SIZE).min(bytes.len());
        let chunk = bytes[offset as usize..end].to_vec();
        match client.caches().nar_upload_chunk(cache, &store_hash, offset, chunk).await {
            Ok(received) if received > offset => offset = received,
            Ok(received) => out.err(
                ExitKind::Api,
                format!("upload stalled for {store_path}: server stayed at {received} of {total}"),
            ),
            Err(e) => out.err(to_exit_kind(&e), e),
        }
    }

    match client.caches().nar_upload_finalize(cache, &store_hash, payload).await {
        Ok(()) => {
            out.ok(&serde_json::json!({"uploaded": true, "store_path": store_path}));
            out.human(format!("Uploaded {store_path}"));
        }
        Err(e) => out.err(to_exit_kind(&e), e),
    }
}

#[cfg(test)]
mod tests {
    use super::store_hash_of;

    #[test]
    fn extracts_hash_from_full_path_and_name_with_specials() {
        assert_eq!(
            store_hash_of("/nix/store/bnq5n76hrfr50l5s2hbbg9vw32fvcrbc-linux-rpi-6.12.75-1+rpt1"),
            "bnq5n76hrfr50l5s2hbbg9vw32fvcrbc"
        );
        assert_eq!(store_hash_of("bnq5n76hrfr50l5s2hbbg9vw32fvcrbc-hello"), "bnq5n76hrfr50l5s2hbbg9vw32fvcrbc");
    }
}
