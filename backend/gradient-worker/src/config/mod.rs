/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod options;

pub use options::*;

use std::num::{NonZeroU32, NonZeroUsize};
use std::path::PathBuf;
use std::time::Duration;

use gradient_eval::pool::MemoryGuard;
use gradient_secret::SecretString;
use gradient_vm::{SupervisorConfig, VmConfig};
use gradient_wire::types::{ExecAllow, ExecDefaults, GradientCapabilities, GuestSize, PortRanges};
use ipnet::Ipv6Net;

pub fn host_system() -> String {
    let arch = std::env::consts::ARCH;
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    format!("{arch}-{os}")
}

impl WorkerConfig {
    pub fn peer_tokens(&self) -> Vec<(String, SecretString)> {
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
                Some((peer_id, SecretString::from(token)))
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

    pub fn nix_daemon_max_connections(&self) -> NonZeroUsize {
        self.nix_daemon.max_connections.unwrap_or_else(|| {
            let builds = if self.capabilities.build {
                self.build.max_concurrent.get() as usize
            } else {
                0
            };
            NonZeroUsize::new(builds * 9 + 16).expect("The sum of 16 and the product is not zero.")
        })
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

    pub fn guests_dir(&self) -> PathBuf {
        self.base_dir.join("guests")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostResources {
    pub cpus: u32,
    pub memory_mb: u64,
    pub guests_dir_free_mb: u64,
}

const DEFAULT_BUDGET_PERCENT: u64 = 80;

fn budget_share(amount: u64) -> u64 {
    amount / 100 * DEFAULT_BUDGET_PERCENT + amount % 100 * DEFAULT_BUDGET_PERCENT / 100
}

impl ExecArgs {
    pub fn guest_budget(&self, host: &HostResources) -> GuestSize {
        GuestSize {
            cpus: self.guest_cpus.unwrap_or(host.cpus),
            memory_mb: self
                .guest_memory_mb
                .unwrap_or_else(|| budget_share(host.memory_mb)),
            disk_mb: self
                .guest_disk_mb
                .unwrap_or_else(|| budget_share(host.guests_dir_free_mb)),
        }
    }

    pub fn allow(&self) -> ExecAllow {
        ExecAllow {
            network: self.allow_network,
            ssh: self.allow_ssh,
            ports: self
                .allow_ports
                .clone()
                .unwrap_or_else(PortRanges::unprivileged),
            nodes: self.allow_nodes,
            devices: self.allow_devices.clone(),
            mounts: self.allow_mounts.clone(),
            custom_images: self.allow_custom_images,
            timeout_secs: self.allow_timeout,
            max_silent_secs: self.allow_max_silent,
            retries: self.allow_retries,
        }
    }

    pub fn defaults(&self) -> ExecDefaults {
        ExecDefaults {
            network: self.defaults_network,
            ssh: self.defaults_ssh,
            mounts: self.defaults_mounts.clone(),
            max_silent_secs: self.defaults_max_silent,
        }
    }

    pub fn vm_config(&self, guests_dir: PathBuf) -> VmConfig {
        VmConfig {
            image: self.image.clone().unwrap_or_default(),
            pre_warm: self.pre_warm,
            max_pre_warm: self.max_pre_warm,
            guest_store_mb: self.guest_store_mb,
            devices: self.devices.clone(),
            mounts: self.mounts.clone(),
            bridge: self.bridge.clone(),
            virtiofsd_bin: self.virtiofsd_bin.clone(),
            guests_dir,
        }
    }

    pub fn parse_exec_subnet_v6(&self) -> anyhow::Result<Option<Ipv6Net>> {
        let Some(subnet_v6) = self.subnet_v6.as_deref() else {
            return Ok(None);
        };
        let Some(subnet_v4) = self.subnet_v4.as_deref() else {
            anyhow::bail!("Set exec.subnet.v4 to use exec.subnet.v6.");
        };
        let guest_bridge_ipv4_subnet = gradient_vm::parse_guest_subnet(subnet_v4)?;
        gradient_vm::parse_guest_subnet_v6(subnet_v6, guest_bridge_ipv4_subnet).map(Some)
    }

    pub fn supervisor_config(&self) -> anyhow::Result<SupervisorConfig> {
        let guest_bridge_ipv4_subnet = self
            .subnet_v4
            .as_deref()
            .map(gradient_vm::parse_guest_subnet)
            .transpose()?;
        Ok(SupervisorConfig {
            virtiofsd_bin: self.virtiofsd_bin.clone(),
            devices: self.devices.clone(),
            mounts: self.mounts.clone(),
            bridge: self.bridge.clone(),
            guest_bridge_ipv4_subnet,
            guest_bridge_ipv6_subnet: self.parse_exec_subnet_v6()?,
        })
    }
}

pub const REMOVED_GRADIENT_WORKER_EXEC_SUBNET_ENV_VAR_NAME: &str = "GRADIENT_WORKER_EXEC_SUBNET";

pub fn fail_on_removed_exec_subnet_env_var(
    removed_exec_subnet_env_var_value: Option<&std::ffi::OsStr>,
) -> anyhow::Result<()> {
    if removed_exec_subnet_env_var_value.is_some() {
        anyhow::bail!(
            "The worker won't start when the removed `GRADIENT_WORKER_EXEC_SUBNET` environment variable is set. Rename it to `GRADIENT_WORKER_EXEC_SUBNET_V4`."
        );
    }
    Ok(())
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
    use clap::{CommandFactory, Parser};

    fn parse(args: &[&str]) -> Result<WorkerConfig, clap::Error> {
        WorkerConfig::try_parse_from(std::iter::once("gradient-worker").chain(args.iter().copied()))
    }

    #[test]
    fn devices_come_from_a_json_environment_value() {
        let config = parse(&[
            "--exec-devices",
            r#"[{"name":"gpu:h100","projects":"all","vfio":["0000:17:00.0","0000:18:00.0"]},{"name":"camera","projects":[],"usb":"1d6b:0002"}]"#,
        ])
        .expect("valid devices");

        assert_eq!(
            config.exec.devices,
            vec![
                gradient_vm::DeviceConfig {
                    name: "gpu:h100".into(),
                    projects: gradient_wire::host_resource_project_access::WorkerResourceProjectAccess::EveryProject,
                    vfio: vec!["0000:17:00.0".into(), "0000:18:00.0".into()],
                    dev: None,
                    usb: None,
                },
                gradient_vm::DeviceConfig {
                    name: "camera".into(),
                    projects: gradient_wire::host_resource_project_access::WorkerResourceProjectAccess::OnlyProjects(Default::default()),
                    vfio: Vec::new(),
                    dev: None,
                    usb: Some("1d6b:0002".into()),
                },
            ]
        );
    }

    #[test]
    fn json_list_with_two_sops_files_parses_into_two_entries_with_paths_and_projects() {
        let second_sops_file_project_uuid = uuid::Uuid::now_v7();
        let sops_files_json_text_with_two_entries = format!(
            r#"[{{"path":"/a.yaml","projects":"all"}},{{"path":"/b.yaml","projects":["{second_sops_file_project_uuid}"]}}]"#
        );
        let config = parse(&["--exec-sops-files", &sops_files_json_text_with_two_entries])
            .expect("valid entries");
        assert_eq!(
            config.exec.sops_files,
            [
                gradient_vm::SopsSecretFileEntry {
                    path: PathBuf::from("/a.yaml"),
                    projects: gradient_wire::host_resource_project_access::WorkerResourceProjectAccess::EveryProject,
                },
                gradient_vm::SopsSecretFileEntry {
                    path: PathBuf::from("/b.yaml"),
                    projects: gradient_wire::host_resource_project_access::WorkerResourceProjectAccess::OnlyProjects(
                        [second_sops_file_project_uuid].into()
                    ),
                },
            ]
        );
    }

    #[test]
    fn sops_file_entry_without_projects_is_rejected_with_entry_name() {
        let sops_file_entry_without_projects_parse_error_text = parse(&[
            "--exec-sops-files",
            r#"[{"path":"/a.yaml","projects":"all"},{"path":"/b.yaml"}]"#,
        ])
        .expect_err("entry without projects")
        .to_string();
        assert!(
            sops_file_entry_without_projects_parse_error_text.contains("entry 1")
                && sops_file_entry_without_projects_parse_error_text.contains("projects"),
            "{sops_file_entry_without_projects_parse_error_text}"
        );
    }

    #[test]
    fn rejects_old_comma_separated_sops_file_path_list() {
        assert!(parse(&["--exec-sops-files", "/a.yaml,/b.yaml"]).is_err());
        assert!(parse(&["--exec-sops-files", "/a.yaml"]).is_err());
    }

    #[test]
    fn a_reversed_port_range_is_refused() {
        assert!(parse(&["--exec-allow-ports", "65535-1024"]).is_err());
    }

    #[test]
    fn allowed_ports_mix_single_ports_and_ranges() {
        let port = |first, last| gradient_wire::types::PortRange { first, last };
        let config = parse(&["--exec-allow-ports", "22,8000-8100,6006"]).expect("valid ports");
        assert_eq!(
            config.exec.allow().ports,
            PortRanges(vec![port(22, 22), port(8000, 8100), port(6006, 6006)])
        );
        let none = parse(&["--exec-allow-ports", ""]).expect("an empty list");
        assert_eq!(none.exec.allow().ports, PortRanges::default());
        let unset = parse(&[]).expect("defaults");
        assert_eq!(unset.exec.allow().ports, PortRanges::unprivileged());
    }

    #[test]
    fn custom_images_follow_allow_custom_images() {
        let allowed = parse(&["--exec-allow-custom-images"]).expect("valid flag");
        assert!(allowed.exec.allow().custom_images);
        assert!(!parse(&[]).expect("defaults").exec.allow().custom_images);
    }

    #[test]
    fn the_guest_budget_defaults_follow_the_host() {
        let host = HostResources {
            cpus: 16,
            memory_mb: 64_000,
            guests_dir_free_mb: 100_000,
        };
        let defaults = parse(&[]).expect("defaults");
        assert_eq!(
            defaults.exec.guest_budget(&host),
            GuestSize {
                cpus: 16,
                memory_mb: 51_200,
                disk_mb: 80_000
            }
        );
        let set = parse(&[
            "--exec-guest-cpus",
            "2",
            "--exec-guest-memory-mb",
            "1000",
            "--exec-guest-disk-mb",
            "4096",
        ])
        .expect("valid flags");
        assert_eq!(
            set.exec.guest_budget(&host),
            GuestSize {
                cpus: 2,
                memory_mb: 1000,
                disk_mb: 4096
            }
        );
    }

    #[test]
    fn a_device_with_two_sources_is_refused() {
        assert!(
            parse(&[
                "--exec-devices",
                r#"[{"name":"gpu","vfio":["0000:17:00.0"],"dev":"/dev/kvm"}]"#,
            ])
            .is_err()
        );
    }

    #[test]
    fn worker_supervisor_config_includes_exec_subnet_v4_and_v6_from_env() {
        let command = WorkerConfig::command();
        for (long, env) in [
            ("exec-subnet-v4", "GRADIENT_WORKER_EXEC_SUBNET_V4"),
            ("exec-subnet-v6", "GRADIENT_WORKER_EXEC_SUBNET_V6"),
        ] {
            let arg = command
                .get_arguments()
                .find(|arg| arg.get_long() == Some(long))
                .unwrap_or_else(|| panic!("--{long}"));
            assert_eq!(arg.get_env(), Some(std::ffi::OsStr::new(env)));
        }

        let config = parse(&[
            "--exec-subnet-v4",
            "10.233.0.0/16",
            "--exec-subnet-v6",
            "fd00:ec2::/64",
        ])
        .unwrap();
        let supervisor_config = config.exec.supervisor_config().unwrap();
        assert_eq!(
            supervisor_config.guest_bridge_ipv4_subnet,
            Some("10.233.0.0/16".parse().unwrap())
        );
        assert_eq!(
            supervisor_config.guest_bridge_ipv6_subnet,
            Some("fd00:ec2::/64".parse().unwrap())
        );
    }

    #[test]
    fn supervisor_config_rejects_ipv6_subnet_when_ipv4_subnet_missing() {
        let config = parse(&["--exec-subnet-v6", "fd00:ec2::/64"]).unwrap();
        assert!(config.exec.supervisor_config().is_err());
        assert!(config.exec.parse_exec_subnet_v6().is_err());
    }

    #[test]
    fn worker_start_check_rejects_old_subnet_variable() {
        let error = fail_on_removed_exec_subnet_env_var(Some("10.233.0.0/16".as_ref()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("`GRADIENT_WORKER_EXEC_SUBNET`"), "{error}");
        assert!(
            error.contains("`GRADIENT_WORKER_EXEC_SUBNET_V4`"),
            "{error}"
        );
        assert!(fail_on_removed_exec_subnet_env_var(None).is_ok());
    }

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
                ("peer1".to_owned(), SecretString::from(TOK64)),
                ("peer2".to_owned(), SecretString::from(TOK64))
            ]
        );

        let cfg = config_with_peers(&format!("\n# a comment line\npeer:{TOK64}\n\n"));
        assert_eq!(
            cfg.peer_tokens(),
            vec![("peer".to_owned(), SecretString::from(TOK64))]
        );

        for bad in [TOK63, TOK65] {
            let cfg = config_with_peers(&format!("peer-bad:{bad}\npeer-ok:{TOK64}"));
            assert_eq!(
                cfg.peer_tokens(),
                vec![("peer-ok".to_owned(), SecretString::from(TOK64))],
                "{bad}"
            );
        }

        let cfg = config_with_peers(&format!(":tok64ok\npeer:\nnocolon\npeer-valid:{TOK64}"));
        assert_eq!(
            cfg.peer_tokens(),
            vec![("peer-valid".to_owned(), SecretString::from(TOK64))]
        );

        let cfg = config_with_peers(&format!("*:{TOK64}"));
        assert_eq!(
            cfg.peer_tokens(),
            vec![("*".to_owned(), SecretString::from(TOK64))]
        );
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
        assert_eq!(
            tokens,
            vec![("peer-file".to_owned(), SecretString::from(TOK64))]
        );
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

    #[test]
    fn daemon_connection_pool_size_is_16_when_builds_disabled_and_unconfigured() {
        let mut config = parse(&["--build-max-concurrent", "32"]).expect("defaults");
        config.capabilities.build = false;
        assert_eq!(config.nix_daemon_max_connections().get(), 16);
    }

    #[test]
    fn daemon_connection_pool_size_is_build_limit_plus_16_unless_configured() {
        let mut config = parse(&["--build-max-concurrent", "32"]).expect("defaults");
        assert!(config.capabilities.build);
        assert_eq!(config.nix_daemon_max_connections().get(), 304);

        config.nix_daemon.max_connections = NonZeroUsize::new(40);
        assert_eq!(config.nix_daemon_max_connections().get(), 40);
        config.capabilities.build = false;
        assert_eq!(config.nix_daemon_max_connections().get(), 40);
    }
}
