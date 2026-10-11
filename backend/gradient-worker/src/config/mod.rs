/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod options;

pub use options::*;

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
                    tracing::warn!(path, error = %e, "failed to read peers file; connecting in open mode");
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
                tracing::warn!(path, error = %e, "failed to read accepted server tokens file; rejecting every server");
                Some(Vec::new())
            }
        }
    }

    pub fn nar_partial_dir(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.base_dir).join("nar-partial")
    }

    pub fn eval_cache_dir(&self) -> String {
        self.eval
            .cache_dir
            .clone()
            .unwrap_or_else(|| format!("{}/eval-cache", self.base_dir))
    }

    pub fn build_cores(&self) -> u32 {
        self.build.max_cores.unwrap_or(0)
    }

    pub fn cpu_core_score(&self) -> u32 {
        self.system
            .cpu_core_score
            .unwrap_or_else(crate::metrics::cpu_core_score)
    }

    pub fn capabilities(&self) -> GradientCapabilities {
        GradientCapabilities {
            core: false,
            federate: self.capabilities.federate,
            fetch: self.capabilities.fetch,
            eval: self.capabilities.eval,
            build: self.capabilities.build,
            cache: false,
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
    fn peer_tokens_from_inline_string() {
        let cfg = config_with_peers(&format!("peer1:{TOK64}\npeer2:{TOK64}"));
        let tokens = cfg.peer_tokens();
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0], ("peer1".to_owned(), TOK64.to_owned()));
        assert_eq!(tokens[1], ("peer2".to_owned(), TOK64.to_owned()));
    }

    #[test]
    fn peer_tokens_skips_blank_lines_and_comments() {
        let input = format!("\n# this is a comment\npeer:{TOK64}\n\n");
        let cfg = config_with_peers(&input);
        let tokens = cfg.peer_tokens();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, "peer");
    }

    #[test]
    fn peer_tokens_skips_short_tokens() {
        let input = format!("peer-short:{TOK63}\npeer-ok:{TOK64}");
        let cfg = config_with_peers(&input);
        let tokens = cfg.peer_tokens();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, "peer-ok");
    }

    #[test]
    fn peer_tokens_skips_long_tokens() {
        let input = format!("peer-long:{TOK65}\npeer-ok:{TOK64}");
        let cfg = config_with_peers(&input);
        let tokens = cfg.peer_tokens();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, "peer-ok");
    }

    #[test]
    fn peer_tokens_empty_when_neither_set() {
        let cfg = WorkerConfig::default();
        assert!(cfg.peer_tokens().is_empty());
    }

    #[test]
    fn peer_tokens_skips_empty_peer_or_token() {
        let input = format!(":tok64ok\npeer:\nnocolon\npeer-valid:{TOK64}");
        let cfg = config_with_peers(&input);
        let tokens = cfg.peer_tokens();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, "peer-valid");
    }

    #[test]
    fn peer_tokens_preserves_wildcard() {
        let input = format!("*:{TOK64}");
        let cfg = config_with_peers(&input);
        let tokens = cfg.peer_tokens();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0], ("*".to_owned(), TOK64.to_owned()));
    }

    #[test]
    fn peer_tokens_from_file() {
        let path = std::env::temp_dir()
            .join(format!("gradient-test-peers-{}", std::process::id()))
            .to_str()
            .unwrap()
            .to_owned();
        std::fs::write(&path, format!("peer-file:{TOK64}")).unwrap();

        let cfg = WorkerConfig {
            peers: Some(format!("peer-inline:{TOK64}")),
            peers_file: Some(path.clone()),
            ..Default::default()
        };
        let tokens = cfg.peer_tokens();
        let _ = std::fs::remove_file(&path);
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, "peer-file");
    }

    #[test]
    fn accepted_server_tokens_are_absent_without_a_file() {
        assert!(WorkerConfig::default().accepted_server_tokens().is_none());
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
            accepted_server_tokens_file: Some(path.to_string_lossy().into_owned()),
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
    fn an_unreadable_accepted_server_tokens_file_rejects_every_server() {
        let cfg = WorkerConfig {
            accepted_server_tokens_file: Some(
                "/nonexistent/gradient-accepted-server-tokens".into(),
            ),
            ..Default::default()
        };
        assert_eq!(cfg.accepted_server_tokens(), Some(vec![]));
    }
}
