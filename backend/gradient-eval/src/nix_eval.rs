/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::ffi::OsString;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use nix_bindings::flake::{FetchersSettings, FlakeSettings, LockedFlake};
use nix_bindings::{Context, EvalState, EvalStateBuilder, Store};

pub use nix_bindings::{FetchHook, RealiseHook};

pub type FlakeLockCheckFn = fn(
    lock_file: &serde_json::Value,
    flake_dir: Option<&str>,
    override_references: &[&str],
) -> std::result::Result<(), String>;

pub struct NixEvaluator {
    ctx: Arc<Context>,
    store: Arc<Store>,
    flake_settings: Arc<FlakeSettings>,
    fetch_settings: FetchersSettings,
    state: EvalState,
    nix_computed_flake_lock_check: Option<FlakeLockCheckFn>,
}

const FETCH_REQUEST: &str = r#"json:
  let
    request = builtins.fromJSON json;
    fetch = builtins.getAttr request.builtin { inherit (builtins) fetchTree fetchTarball fetchurl; };
    isGitTree = request.builtin == "fetchTree" && (request.args.type or null) == "git";
    fetched = fetch (if isGitTree then { shallow = false; } // request.args else request.args);
    locked = builtins.intersectAttrs { narHash = null; rev = null; revCount = null; lastModified = null; } fetched;
  in
  builtins.toJSON (
    if request.builtin == "fetchTree"
    then { storePath = fetched.outPath; attrs = request.args // locked; }
    else { storePath = fetched; }
  )"#;

pub struct StatsReader<'ev>(&'ev EvalState);

// SAFETY: `stats_with` reads only atomic counters and `GC_get_heap_usage_safe`, so a reader may run beside the evaluating thread.
unsafe impl Send for StatsReader<'_> {}

impl StatsReader<'_> {
    pub fn read(&self, ctx: &Context) -> Result<nix_bindings::EvalStats> {
        Ok(self.0.stats_with(ctx)?)
    }
}

#[cfg(target_os = "linux")]
fn ensure_store_writable() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let store = c"/nix/store".as_ptr();
        if libc::geteuid() != 0 {
            return;
        }
        let mut vfs: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(store, &mut vfs) != 0 || (vfs.f_flag & libc::ST_RDONLY) == 0 {
            return;
        }
        if libc::unshare(libc::CLONE_NEWNS) != 0 {
            return;
        }
        libc::mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        );
        let flags = [
            (libc::ST_NODEV, libc::MS_NODEV),
            (libc::ST_NOSUID, libc::MS_NOSUID),
            (libc::ST_NOEXEC, libc::MS_NOEXEC),
            (libc::ST_NOATIME, libc::MS_NOATIME),
            (libc::ST_NODIRATIME, libc::MS_NODIRATIME),
            (libc::ST_RELATIME, libc::MS_RELATIME),
        ]
        .into_iter()
        .fold(libc::MS_REMOUNT | libc::MS_BIND, |acc, (st, ms)| {
            if (vfs.f_flag & st) != 0 {
                acc | ms
            } else {
                acc
            }
        });
        libc::mount(
            std::ptr::null(),
            store,
            std::ptr::null(),
            flags,
            std::ptr::null(),
        );
    });
}

#[cfg(not(target_os = "linux"))]
fn ensure_store_writable() {}

impl NixEvaluator {
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn new(
        realise_hook: Option<RealiseHook>,
        fetch_hook: Option<FetchHook>,
        nix_computed_flake_lock_check: Option<FlakeLockCheckFn>,
    ) -> Result<Self> {
        ensure_store_writable();
        let ctx = Arc::new(Context::new().context("nix context init")?);
        ctx.set_setting("show-trace", "true")?;
        ctx.set_setting("builders", "")?;
        ctx.set_setting("substitute", "false")?;

        let store = Arc::new(Store::open(&ctx, None).context("nix store open")?);
        let flake_settings = Arc::new(FlakeSettings::new(&ctx)?);
        let fetch_settings = FetchersSettings::new(&ctx)?;

        let mut builder = EvalStateBuilder::new(&store)?
            .with_flake_settings(&flake_settings)?
            .set_setting("eval-cache", "true")?
            .set_setting("pure-eval", "true")?;
        if let Some(hook) = realise_hook {
            builder = builder.set_realise_hook(hook)?;
        }
        if let Some(hook) = fetch_hook {
            builder = builder.set_fetch_hook(hook)?;
        }
        let state = builder.build().context("nix eval state build")?;

        Ok(NixEvaluator {
            ctx,
            store,
            flake_settings,
            fetch_settings,
            state,
            nix_computed_flake_lock_check,
        })
    }

    pub fn set_restrict_eval_and_allowed_uris(&self, allowed_uris: &[String]) -> Result<()> {
        self.state
            .set_evaluator_setting("allowed-uris", &allowed_uris.join(" "))?;
        self.state.set_evaluator_setting("restrict-eval", "true")?;
        Ok(())
    }

    pub fn stats(&self) -> Result<nix_bindings::EvalStats> {
        self.state
            .stats()
            .map_err(|e| anyhow::anyhow!("eval stats: {e}"))
    }

    pub fn stats_reader(&self) -> StatsReader<'_> {
        StatsReader(&self.state)
    }

    pub fn fetch_tree(
        &self,
        request: &str,
        git_subprocess_environment: &[(String, String)],
    ) -> Result<String> {
        self.ctx.set_log_format("internal-json")?;
        let _restore = NixFetchEnvironmentVariablesGuard::set(git_subprocess_environment);
        let fetched = self.answer_fetch(request);
        self.ctx.set_log_format("raw-with-logs")?;
        fetched
    }

    fn answer_fetch(&self, request: &str) -> Result<String> {
        let fetch = self.state.eval_from_string(FETCH_REQUEST, "/")?;
        let json = self.state.make_string(request)?;
        Ok(fetch.call(&json)?.as_string()?)
    }

    pub fn walker(
        &self,
        flake_ref: &str,
        overrides: &[(String, String)],
    ) -> Result<crate::flake_walk::FlakeWalker<'_>> {
        let locked = self
            .lock_flake_with_overrides_and_run_flake_lock_allowlist_check(flake_ref, overrides)?;
        crate::flake_walk::FlakeWalker::open(&self.ctx, &self.state, locked)
    }

    pub fn fingerprint(
        &self,
        flake_ref: &str,
        overrides: &[(String, String)],
    ) -> Result<Option<String>> {
        let locked = self
            .lock_flake_with_overrides_and_run_flake_lock_allowlist_check(flake_ref, overrides)?;
        Ok(locked.fingerprint(&self.store, &self.fetch_settings)?)
    }

    fn lock_flake_with_overrides_and_run_flake_lock_allowlist_check(
        &self,
        flake_ref: &str,
        overrides: &[(String, String)],
    ) -> Result<LockedFlake> {
        let locked = crate::flake_walk::lock_flake(
            &self.ctx,
            &self.fetch_settings,
            &self.flake_settings,
            &self.state,
            flake_ref,
            overrides,
        )?;
        if let Some(check) = self.nix_computed_flake_lock_check {
            crate::flake_walk::reject_flake_lock_with_disallowed_input(
                &locked, flake_ref, overrides, check,
            )?;
        }
        Ok(locked)
    }
}

struct NixFetchEnvironmentVariablesGuard(Vec<(String, Option<OsString>)>);

impl NixFetchEnvironmentVariablesGuard {
    #[expect(
        clippy::disallowed_methods,
        reason = "Nix fetchers run git as a subprocess and git processes read config from the environment of this process"
    )]
    fn set(variables: &[(String, String)]) -> Self {
        let previous = variables
            .iter()
            .map(|(name, value)| {
                let previous = std::env::var_os(name);
                // SAFETY (set and remove): the stats ticker never reads the environment and nix's curl thread
                // reads proxy variables only inside a transfer, which starts and ends within this fetch.
                unsafe { std::env::set_var(name, value) };
                (name.clone(), previous)
            })
            .collect();
        Self(previous)
    }
}

impl Drop for NixFetchEnvironmentVariablesGuard {
    #[expect(
        clippy::disallowed_methods,
        reason = "Nix fetchers run git as a subprocess and git processes read config from the environment of this process"
    )]
    fn drop(&mut self) {
        for (name, previous) in self.0.drain(..).rev() {
            match previous {
                Some(value) => unsafe { std::env::set_var(&name, value) },
                None => unsafe { std::env::remove_var(&name) },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refuse_absolute_path_nodes(
        lock_file: &serde_json::Value,
        _: Option<&str>,
        _: &[&str],
    ) -> std::result::Result<(), String> {
        let absolute = lock_file["nodes"]
            .as_object()
            .into_iter()
            .flat_map(|nodes| nodes.values())
            .filter_map(|node| node["locked"]["path"].as_str())
            .find(|path| path.starts_with('/'));
        match absolute {
            Some(path) => Err(format!("absolute path input {path}")),
            None => Ok(()),
        }
    }

    fn first_missing_nix_requirement(
        also_run_nix_command_line_binary: bool,
    ) -> Option<&'static str> {
        use std::os::unix::ffi::OsStringExt;
        use std::path::PathBuf;

        #[expect(
            clippy::disallowed_methods,
            reason = "PATH, NIX_REMOTE, NIX_STATE_DIR and NIX_DAEMON_SOCKET_PATH are variables of nix."
        )]
        let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        let path = var("PATH").unwrap_or_default();
        if also_run_nix_command_line_binary
            && !std::env::split_paths(&path).any(|dir| dir.join("nix").is_file())
        {
            return Some("skipped: no nix binary in PATH");
        }
        if var("NIX_REMOTE").is_some() {
            return None;
        }
        let state_dir =
            var("NIX_STATE_DIR").map_or_else(|| PathBuf::from("/nix/var/nix"), PathBuf::from);
        let daemon_socket = var("NIX_DAEMON_SOCKET_PATH")
            .map_or_else(|| state_dir.join("daemon-socket/socket"), PathBuf::from);
        let state_dir_writable = std::ffi::CString::new(state_dir.into_os_string().into_vec())
            .is_ok_and(
            |state_dir| unsafe { libc::access(state_dir.as_ptr(), libc::R_OK | libc::W_OK) } == 0,
        );
        if daemon_socket.exists() || state_dir_writable {
            None
        } else {
            Some(
                "skipped: This user can't open the Nix store (no daemon socket, no write access to the Nix state directory)",
            )
        }
    }

    #[test]
    fn flake_ref_to_string_renders_locked_git_scp_like_and_remote_mercurial_urls_with_scheme_from_allowed_remote_scheme_list_for_restrict_eval_allowed_uris()
     {
        if let Some(missing_nix_requirement_skip_line) = first_missing_nix_requirement(false) {
            eprintln!("{missing_nix_requirement_skip_line}");
            return;
        }
        let ev = NixEvaluator::new(None, None, None).unwrap();
        let exported = serde_json::json!({
            "lockFile": { "nodes": {
                "root": { "inputs": { "scp": "scp", "hgs": "hgs", "hgssh": "hgssh" } },
                "scp": { "locked": { "type": "git", "url": "git@github.com:o/r", "rev": "1111111111111111111111111111111111111111" } },
                "hgs": { "locked": { "type": "hg", "url": "https://hg.example.com/r", "rev": "1111111111111111111111111111111111111111" } },
                "hgssh": { "locked": { "type": "hg", "url": "ssh://hg@hg.example.com/r", "rev": "1111111111111111111111111111111111111111" } },
            } }
        });
        let urls = ev
            .state
            .eval_from_string(
                crate::flake_walk::LOCKED_INPUT_FETCH_URLS_NIX_EXPRESSION,
                "/",
            )
            .unwrap()
            .call(&ev.state.make_string(&exported.to_string()).unwrap())
            .unwrap()
            .as_string()
            .unwrap();
        let mut urls: Vec<&str> = urls.split_whitespace().collect();
        urls.sort();

        assert_eq!(
            urls,
            vec![
                "git+ssh://git@github.com/o/r?rev=1111111111111111111111111111111111111111&shallow=1",
                "hg+https://hg.example.com/r?rev=1111111111111111111111111111111111111111",
                "hg+ssh://hg@hg.example.com/r?rev=1111111111111111111111111111111111111111",
            ]
        );
    }

    #[test]
    fn a_flake_nix_only_path_input_naming_a_host_file_is_readable_without_the_lock_check_and_refused_with_the_allowlist_check()
     {
        if let Some(missing_nix_requirement_skip_line) = first_missing_nix_requirement(true) {
            eprintln!("{missing_nix_requirement_skip_line}");
            return;
        }
        let dir = std::env::temp_dir().join(format!("gradient-eval-lock-{}", std::process::id()));
        let flake = dir.join("flake");
        std::fs::create_dir_all(&flake).unwrap();
        let host_file = dir.join("host-file");
        std::fs::write(&host_file, "host content").unwrap();
        let nar_hash = std::process::Command::new("nix")
            .args([
                "--extra-experimental-features",
                "nix-command",
                "hash",
                "path",
            ])
            .arg(&host_file)
            .output()
            .unwrap();
        let nar_hash = String::from_utf8(nar_hash.stdout).unwrap();
        let write_flake = |url: String| {
            std::fs::write(
                flake.join("flake.nix"),
                format!(
                    r#"{{ inputs.s = {{ url = "{url}"; flake = false; }}; outputs = {{ s, ... }}: {{ }}; }}"#
                ),
            )
            .unwrap();
        };
        let flake_ref = format!("path:{}", flake.display());
        let unchecked = NixEvaluator::new(None, None, None).unwrap();
        let checked = NixEvaluator::new(None, None, Some(refuse_absolute_path_nodes)).unwrap();

        write_flake(format!("path:{}", host_file.display()));
        let unlocked = unchecked
            .lock_flake_with_overrides_and_run_flake_lock_allowlist_check(&flake_ref, &[])
            .err()
            .expect("Nix refuses to lock an unlocked input in pure mode");
        assert!(
            format!("{unlocked:#}").contains("cannot update unlocked flake input"),
            "{unlocked:#}"
        );

        write_flake(format!(
            "path:{}?narHash={}",
            host_file.display(),
            nar_hash.trim().replace('+', "%2B").replace('=', "%3D")
        ));
        let locked = unchecked
            .lock_flake_with_overrides_and_run_flake_lock_allowlist_check(&flake_ref, &[])
            .unwrap();
        assert!(
            locked
                .export_json()
                .unwrap()
                .contains(&*host_file.to_string_lossy())
        );
        let err = checked.walker(&flake_ref, &[]).err().expect("refused");
        assert!(
            format!("{err:#}").contains("disallowed input: absolute path input"),
            "{err:#}"
        );

        std::fs::write(
            flake.join("flake.lock"),
            r#"{"nodes":{"root":{}},"root":"root","version":7}"#,
        )
        .unwrap();
        let stale = checked.walker(&flake_ref, &[]).err().expect("refused");
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            format!("{stale:#}").contains("disallowed input: absolute path input"),
            "{stale:#}"
        );
    }
}
