/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod options;

pub use options::*;

use std::num::{NonZeroU32, NonZeroUsize};
use std::time::Duration;

use gradient_eval::pool::MemoryGuard;
use gradient_wire::types::GradientCapabilities;

pub fn host_system() -> String {
    let arch = std::env::consts::ARCH;
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    format!("{arch}-{os}")
}

impl WorkerConfig {
    pub fn peer_tokens(&self) -> Vec<(String, String)> {
        let raw = if let Some(path) = &self.peers_file {
            match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "failed to read peers file; connecting in open mode");
                    return vec![];
                }
            }
        } else if let Some(s) = &self.peers {
            s.clone()
        } else {
            return vec![];
        };

        raw.lines()
            .filter_map(|entry| {
                let entry = entry.trim();
                if entry.is_empty() || entry.starts_with('#') {
                    return None;
                }
                let mut parts = entry.splitn(2, ':');
                let peer_id = parts.next()?.trim().to_owned();
                let token = parts.next()?.trim().to_owned();
                if peer_id.is_empty() || token.is_empty() {
                    return None;
                }
                if token.len() != 64 {
                    tracing::warn!(
                        peer_id,
                        token_len = token.len(),
                        "token must be exactly 64 base64 chars (48 bytes); skipping entry"
                    );
                    return None;
                }
                Some((peer_id, token))
            })
            .collect()
    }

    pub fn accepted_server_tokens(&self) -> Option<Vec<(String, String)>> {
        let path = self.accepted_server_tokens_file.as_ref()?;
        match std::fs::read_to_string(path) {
            Ok(raw) => Some(parse_token_hashes(&raw)),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "failed to read accepted server tokens file; rejecting every server");
                Some(Vec::new())
            }
        }
    }

    pub fn nar_partial_dir(&self) -> std::path::PathBuf {
        self.base_dir.join("nar-partial")
    }

    #[expect(dead_code, reason = "job tasks adopt it next")]
    pub fn job_log_dir(&self) -> std::path::PathBuf {
        self.base_dir.join("job-logs")
    }

    pub fn eval_cache_dir(&self) -> String {
        self.eval.cache_dir.clone().unwrap_or_else(|| {
            self.base_dir
                .join("eval-cache")
                .to_string_lossy()
                .into_owned()
        })
    }

    pub fn fork_workers(&self) -> NonZeroUsize {
        self.eval
            .fork_workers
            .unwrap_or_else(gradient_eval::pool::default_workers)
    }

    /// The two memory guards are alternatives, not layers: a free-RAM margin or a ceiling on the
    /// resident memory of the whole pool.
    pub fn memory_guard(&self) -> anyhow::Result<MemoryGuard> {
        match (self.system.min_free_ram_mb, self.eval.max_memory) {
            (_, None) => Ok(gradient_eval::pool::min_free_ram_guard(
                self.system.min_free_ram_mb,
            )),
            (0, Some(mib)) => Ok(MemoryGuard::MaxTotal(mib.get() * 1024 * 1024)),
            (mb, Some(_)) => anyhow::bail!(
                "worker.system.minFreeRamMb ({mb}) and worker.eval.maxMemory together; they are \
                 alternatives, unset either"
            ),
        }
    }

    pub fn outbound_server_url(&self) -> anyhow::Result<Option<String>> {
        if self.server_url.is_none() && !self.discoverable {
            anyhow::bail!(
                "worker.serverUrl unset without worker.discoverable; set either to connect to a server"
            );
        }
        Ok(self.server_url.clone())
    }

    pub fn build_cores(&self) -> Option<NonZeroU32> {
        self.build.max_cores
    }

    pub fn cpu_core_score(&self) -> u32 {
        self.system
            .cpu_core_score
            .map_or_else(crate::metrics::cpu_core_score, NonZeroU32::get)
    }

    pub fn exit_limits(&self) -> crate::exit_clock::ExitLimits {
        crate::exit_clock::ExitLimits {
            idle_timeout: self.idle_timeout.map(Duration::from_secs),
            max_lifetime: self.max_lifetime.map(Duration::from_secs),
        }
    }

    pub fn capabilities(&self) -> GradientCapabilities {
        GradientCapabilities {
            core: false,
            federate: self.capabilities.federate,
            eval: self.capabilities.eval,
            build: self.capabilities.build,
            cache: false,
            network: self.capabilities.network,
            secrets: self.capabilities.secrets,
            exec: self.capabilities.exec,
        }
    }
}

fn parse_token_hashes(raw: &str) -> Vec<(String, String)> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once(':'))
        .map(|(peer, hash)| (peer.trim().to_owned(), hash.trim().to_owned()))
        .filter(|(peer, hash)| !peer.is_empty() && !hash.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn checked_in_options_file_is_current() {
        let current =
            gradient_types::cli::options_json(&WorkerConfig::command(), &WorkerConfig::options());

        assert_eq!(
            current,
            include_str!("../../options.json"),
            "run the `options` example of gradient-worker and write its output to gradient-worker/options.json"
        );
    }

    fn config_with_peers(peers: &str) -> WorkerConfig {
        WorkerConfig {
            peers: Some(peers.to_owned()),
            ..Default::default()
        }
    }

    const TOK64: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
    const TOK63: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
    const TOK65: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";

    #[test]
    fn arguments_are_unique() {
        WorkerConfig::command().debug_assert();
    }

    #[test]
    fn every_env_is_the_prefixed_flag() {
        let mismatched: Vec<String> = WorkerConfig::command()
            .get_arguments()
            .filter_map(|arg| {
                let long = arg.get_long()?;
                if matches!(long, "help" | "version") {
                    return None;
                }

                let expected = format!("GRADIENT_WORKER_{}", long.to_uppercase().replace('-', "_"));
                let env = arg.get_env().map(|e| e.to_string_lossy().into_owned());
                (env.as_deref() != Some(expected.as_str())).then(|| format!("--{long}: {env:?}"))
            })
            .collect();

        assert!(mismatched.is_empty(), "{mismatched:#?}");
    }

    #[test]
    fn peer_tokens_keeps_only_well_formed_lines() {
        assert!(WorkerConfig::default().peer_tokens().is_empty());

        let cfg = config_with_peers(&format!("peer1:{TOK64}\npeer2:{TOK64}"));
        assert_eq!(
            cfg.peer_tokens(),
            vec![
                ("peer1".to_owned(), TOK64.to_owned()),
                ("peer2".to_owned(), TOK64.to_owned())
            ]
        );

        let cfg = config_with_peers(&format!("\n# a comment line\npeer:{TOK64}\n\n"));
        assert_eq!(
            cfg.peer_tokens(),
            vec![("peer".to_owned(), TOK64.to_owned())]
        );

        for bad in [TOK63, TOK65] {
            let cfg = config_with_peers(&format!("peer-bad:{bad}\npeer-ok:{TOK64}"));
            assert_eq!(
                cfg.peer_tokens(),
                vec![("peer-ok".to_owned(), TOK64.to_owned())],
                "{bad}"
            );
        }

        let cfg = config_with_peers(&format!(":tok64ok\npeer:\nnocolon\npeer-valid:{TOK64}"));
        assert_eq!(
            cfg.peer_tokens(),
            vec![("peer-valid".to_owned(), TOK64.to_owned())]
        );

        let cfg = config_with_peers(&format!("*:{TOK64}"));
        assert_eq!(cfg.peer_tokens(), vec![("*".to_owned(), TOK64.to_owned())]);
    }

    #[test]
    fn peer_tokens_from_a_file_replace_the_inline_value() {
        let path = std::env::temp_dir().join(format!("gradient-test-peers-{}", std::process::id()));
        std::fs::write(&path, format!("peer-file:{TOK64}")).unwrap();

        let cfg = WorkerConfig {
            peers: Some(format!("peer-inline:{TOK64}")),
            peers_file: Some(path.clone()),
            ..Default::default()
        };
        let tokens = cfg.peer_tokens();
        let _ = std::fs::remove_file(&path);
        assert_eq!(tokens, vec![("peer-file".to_owned(), TOK64.to_owned())]);
    }

    #[test]
    fn accepted_server_tokens_read_peer_hash_lines() {
        let path =
            std::env::temp_dir().join(format!("gradient-test-accepted-{}", std::process::id()));
        std::fs::write(
            &path,
            "# servers\np1:$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA\n\n*:abc123\nbroken\n",
        )
        .unwrap();
        let cfg = WorkerConfig {
            accepted_server_tokens_file: Some(path.clone()),
            ..Default::default()
        };

        let tokens = cfg.accepted_server_tokens();
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            tokens,
            Some(vec![
                (
                    "p1".to_string(),
                    "$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA".to_string()
                ),
                ("*".to_string(), "abc123".to_string()),
            ])
        );
    }

    #[test]
    fn accepted_server_tokens_need_a_readable_file() {
        assert!(WorkerConfig::default().accepted_server_tokens().is_none());

        let cfg = WorkerConfig {
            accepted_server_tokens_file: Some(
                "/nonexistent/gradient-accepted-server-tokens".into(),
            ),
            ..Default::default()
        };
        assert_eq!(cfg.accepted_server_tokens(), Some(vec![]));
    }

    #[test]
    fn a_worker_without_server_url_needs_discoverable() {
        let mut config = WorkerConfig::default();
        let refused = config
            .outbound_server_url()
            .expect_err("a worker without any connection must be refused");
        let message = refused.to_string();
        assert!(message.contains("serverUrl"), "{message}");
        assert!(message.contains("discoverable"), "{message}");

        config.discoverable = true;
        assert_eq!(config.outbound_server_url().expect("listener only"), None);

        config.server_url = Some("ws://server/proto".to_owned());
        assert_eq!(
            config.outbound_server_url().expect("both directions"),
            Some("ws://server/proto".to_owned())
        );
    }

    #[test]
    fn both_memory_options_together_are_refused() {
        let mut config = WorkerConfig::default();
        config.system.min_free_ram_mb = 512;
        config.eval.max_memory = std::num::NonZeroU64::new(1024);

        let refused = config
            .memory_guard()
            .expect_err("both options must be refused");

        let message = refused.to_string();
        assert!(message.contains("minFreeRamMb"), "{message}");
        assert!(message.contains("maxMemory"), "{message}");
    }

    #[test]
    fn each_memory_option_selects_its_own_guard() {
        let mut config = WorkerConfig::default();
        assert!(matches!(
            config.memory_guard().expect("the default"),
            MemoryGuard::MinFreeRam(_)
        ));

        config.eval.max_memory = std::num::NonZeroU64::new(1024);
        assert_eq!(
            config.memory_guard().expect("the cap"),
            MemoryGuard::MaxTotal(1024 * 1024 * 1024)
        );

        config.eval.max_memory = None;
        config.system.min_free_ram_mb = 512;
        assert_eq!(
            config.memory_guard().expect("the margin"),
            MemoryGuard::MinFreeRam(512 * 1024 * 1024)
        );
    }
}
