/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, config, ... }: let
  cfg = config.services.gradient.worker;
  logLevelType = lib.types.enum [ "trace" "debug" "info" "warn" "error" ];
in {
  options.services.gradient.worker = {
    enable = lib.mkEnableOption "Gradient worker";

    packages = {
      gradient = lib.mkPackageOption pkgs "gradient" { };
      nix = lib.mkOption {
        default = pkgs.gradient-nix;
        defaultText = lib.literalExpression "pkgs.gradient-nix";
        type = lib.types.package;
        description = "Nix package to use for evaluation and fetching. The `nix` binary from this package is passed to the worker as `GRADIENT_WORKER_NIX_BIN`. Defaults to the gradient nix fork so the shelled-out `nix` matches the worker's embedded fork evaluator.";
      };

      git = lib.mkOption {
        default = config.programs.git.package;
        defaultText = lib.literalExpression "config.programs.git.package";
        type = lib.types.package;
        description = "Git package. Required by the worker's repository cloning code (libgit2 may spawn git subprocesses).";
      };

      ssh = lib.mkOption {
        default = config.programs.ssh.package;
        defaultText = lib.literalExpression "config.programs.ssh.package";
        type = lib.types.package;
        description = "OpenSSH package. Passed as GIT_SSH_COMMAND so nix flake archive can fetch private flake inputs.";
      };
    };

    reverseProxy = {
      nginx.enable = lib.mkEnableOption "Nginx reverse proxy for the worker listener";
      caddy = {
        enable = lib.mkEnableOption "Caddy reverse proxy for the worker listener";
        useACMEHost = lib.mkOption {
          description = ''
            A host of an existing Let’s Encrypt certificate to use.

            This options is directly passed to `services.caddy.virtualHosts.<name>.useACMEHost`
            and therefore does not create an ACME certificate.
          '';
          type = lib.types.nullOr lib.types.str;
          default = null;
        };

        extraConfig = lib.mkOption {
          description = ''
            Additional lines of configuration passed to
            `services.caddy.virtualHosts.<name>.extraConfig`
            after the reverse proxy setup.
          '';
          type = lib.types.lines;
          default = "";
        };
      };
    };

    useTls = lib.mkEnableOption "TLS" // { default = true; };

    discoverable = lib.mkEnableOption "incoming connections on `/proto`";

    domain = lib.mkOption {
      description = "Domain under which the worker's nginx vhost is served. Only used when a reverseProxy is enabled";
      type = lib.types.str;
      default = "";
      example = "worker.example.com";
    };

    serverUrl = lib.mkOption {
      description = "WebSocket URL of the Gradient server protocol endpoint";
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "wss://gradient.example.com/proto";
    };

    baseDir = lib.mkOption {
      description = "Base directory for Gradient";
      type = lib.types.path;
      default = "/var/lib/gradient-worker";
    };

    listenAddr = lib.mkOption {
      description = "IP address on which the worker listener binds";
      type = lib.types.str;
      default = "127.0.0.1";
    };

    port = lib.mkOption {
      description = "Port for the worker's listener";
      type = lib.types.port;
      default = 3100;
    };

    id = lib.mkOption {
      description = ''
        Override the worker's persistent UUID. When set, this UUID is used as
        the worker identity instead of the one auto-generated and stored in
        `$StateDirectory/worker-id` on first start. Useful for declarative
        deployments that must know the worker UUID ahead of time (e.g. to
        pre-register it in `state.workers`).
      '';
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "550e8400-e29b-41d4-a716-446655440001";
    };

    peersFile = lib.mkOption {
      description = ''
        Path to a file of peer-to-token pairs for challenge-response auth with
        the Gradient server, one `peer_id:token` per line (lines starting with
        `#` are ignored):

        ```
        <uuid>:<token>
        *:<token>
        ```

        The special peer ID `*` matches any UUID the server challenges, so a
        single token works for any project. Each token is a 48-byte random secret
        (e.g. `openssl rand -base64 48`) registered via
        `POST /api/v1/projects/{project}/workers`. Pin the project UUID with
        `services.gradient.state.projects.<name>.id` to reference it here.

        When null (default), the worker connects in open/discoverable mode and
        the server accepts it without token validation.
      '';
      type = lib.types.nullOr lib.types.path;
      default = null;
    };

    drainTimeoutSecs = lib.mkOption {
      description = ''
        How long a SIGINT/SIGTERM drain waits for in-flight jobs. The worker
        stops accepting work at once, finishes and reports what is running,
        then exits; jobs still running at the deadline are aborted and
        re-queued server-side. `TimeoutStopSec` is derived from this. Set to
        0 to wait without limit: `TimeoutStopSec` is then infinity, so a
        wedged build holds `systemctl stop` until a second signal
        (`systemctl kill -s TERM gradient-worker`) aborts it.
      '';
      type = lib.types.ints.unsigned;
      default = 60;
    };

    gcrootsDir = lib.mkOption {
      description = ''
        Directory under which the worker writes one indirect GC root symlink
        per active build (drv + outputs), pinning inputs and just-built
        outputs so a concurrent `nix-collect-garbage` cannot delete them
        mid-build. Set to an empty string to disable pinning (the worker
        still builds, but a concurrent GC may race it).
      '';
      type = lib.types.str;
      default = "/nix/var/nix/gcroots/gradient";
    };

    capabilities = {
      federate = lib.mkEnableOption "the federate capability (relay work and NAR traffic between workers and servers; requires discoverable)";
      fetch = lib.mkEnableOption "the fetch capability (prefetch flake inputs and sources)" // { default = true; };
      eval  = lib.mkEnableOption "the eval capability (run Nix flake evaluations)" // { default = true; };
      build = lib.mkEnableOption "the build capability (execute Nix store builds)" // { default = true; };
    };

    system = {
      architectures = lib.mkOption {
        description = "Nix system strings this worker can build for";
        type = lib.types.listOf lib.types.str;
        default = [ pkgs.stdenv.hostPlatform.system ] ++ lib.optional (pkgs.stdenv.hostPlatform.system == "x86_64-linux") "i686-linux";
        defaultText = lib.literalExpression ''[ pkgs.stdenv.hostPlatform.system ] ++ lib.optional (pkgs.stdenv.hostPlatform.system == "x86_64-linux") "i686-linux"'';
        example = [ "x86_64-linux" "aarch64-linux" ];
      };

      features = lib.mkOption {
        description = ''
          Nix system features this worker advertises to the scheduler. Empty by
          default, which makes the worker auto-detect them at runtime from
          `nix config show system-features` (the daemon's resolved set,
          including CPU-derived `gccarch-*` levels a static list can't
          enumerate). Set a non-empty list to override.
        '';
        type = lib.types.listOf lib.types.str;
        default = [ ];
        example = [ "nixos-test" "benchmark" "big-parallel" ];
      };

      cpuCoreScore = lib.mkOption {
        description = "Override the advertised single-core speed score (higher is faster). When null, the worker benchmarks the host at startup.";
        type = lib.types.nullOr lib.types.ints.positive;
        default = null;
      };

      minFreeRamMb = lib.mkOption {
        description = "Free-RAM safety margin in MiB for the eval-subprocess reaper. When host MemAvailable falls below this, the worker SIGKILLs the one live eval subprocess holding enough resident memory to bring it back above the margin (the parent then reports the eval failed instead of the machine freezing); when no eval is that large the pressure is not coming from evaluation and nothing is killed. 0 selects an adaptive margin of 10% of total RAM clamped to [128 MiB, 1 GiB]. maxEvalRss still bounds steady-state RSS; this is the proactive peak guard.";
        type = lib.types.ints.unsigned;
        default = 0;
      };
    };

    nixDaemon = {
      maxConnections = lib.mkOption {
        description = ''
          Maximum number of simultaneous local Nix daemon connections in
          the connection pool. Each build holds one for its whole run plus
          up to 8 for parallel NAR imports; the rest is headroom for
          path-presence checks.
        '';
        type = lib.types.ints.positive;
        default = cfg.build.maxConcurrent * 9 + 16;
        defaultText = lib.literalExpression "config.services.gradient.worker.build.maxConcurrent * 9 + 16";
      };
    };

    eval = {
      maxConcurrent = lib.mkOption {
        description = "Maximum number of concurrent evaluations";
        type = lib.types.ints.positive;
        default = 1;
      };

      workers = lib.mkOption {
        description = "Number of Nix evaluator subprocesses";
        type = lib.types.ints.positive;
        default = 8;
      };

      forkWorkers = lib.mkOption {
        description = "Number of parallel eval subprocesses in the pool (the eval concurrency). When null, the worker auto-sizes to the host core count (capped). Each worker may hold up to maxEvalRss of resident memory.";
        type = lib.types.nullOr lib.types.ints.positive;
        default = null;
      };

      maxRss = lib.mkOption {
        description = "Safety cap on an eval subprocess's resident memory: once its RSS exceeds this many bytes it is recycled (parent-side). Keep it above a typical eval's heap so warm workers are not recycled mid-evaluation.";
        type = lib.types.ints.positive;
        default = 8589934592;
      };

      metrics = lib.mkOption {
        description = "Capture per-evaluation Nix metrics (thunks, heap, peak RSS, per-entry-point hotspots, flake graph). When false, eval-workers skip the stats read (zero overhead).";
        type = lib.types.bool;
        default = true;
      };

      cache = {
        dir = lib.mkOption {
          description = "Eval-cache directory exported to eval workers as NIX_CACHE_HOME. When null, resolves to {baseDir}/eval-cache.";
          type = lib.types.nullOr lib.types.str;
          default = null;
        };

        share = lib.mkOption {
          description = "Enable fleet eval-cache sharing (pull/push of <fingerprint>.sqlite blobs across workers).";
          type = lib.types.bool;
          default = true;
        };
      };
    };

    build = {
      maxConcurrent = lib.mkOption {
        description = "Maximum number of concurrent builds";
        type = lib.types.ints.positive;
        default = 32;
      };

      maxCores = lib.mkOption {
        description = ''
          Cap on CPU cores a single build may use (nix `--cores` /
          `NIX_BUILD_CORES`). Null (the default) means all available cores.
        '';
        type = lib.types.nullOr lib.types.ints.positive;
        default = null;
      };

      metrics = lib.mkOption {
        description = ''
          Capture per-build resource metrics (peak RAM, CPU time, disk I/O) by
          enabling Nix's experimental `cgroups` feature and `use-cgroups` on the
          daemon, and delegating the cgroup-v2 controllers to `nix-daemon.service`
          (`Delegate=yes`) so per-build `memory.peak`/`io.stat` are exposed. CPU
          time comes from the daemon build result; peak RAM and disk I/O are
          sampled live from the build's `nix-build@<drv-hash>-<uid>` cgroup, which
          needs the gradient nix fork on the daemon, so `nix.package` defaults to
          `packages.nix`. Wall-clock build time is always reported.
        '';
        type = lib.types.bool;
        default = false;
      };

      cgroupRoot = lib.mkOption {
        description = "The nix daemon's cgroup, in which it creates each build's cgroup when `buildMetrics` is enabled.";
        type = lib.types.str;
        default = "/sys/fs/cgroup/system.slice/nix-daemon.service";
      };
    };

    nar = {
      maxConcurrentUploads = lib.mkOption {
        description = ''
          Maximum number of PUTs to object storage (presigned NAR uploads,
          multipart parts, eval-cache blobs) in flight at once across all
          jobs. Throttled PUTs (503/429) retry with backoff.
        '';
        type = lib.types.ints.positive;
        default = 8;
      };

      partialTtlSecs = lib.mkOption {
        description = ''
          TTL in seconds for partially-received NAR downloads (`*.partial`)
          staged under `<baseDir>/nar-partial`. A periodic sweep deletes
          partials whose last write is older than this so an abandoned
          resumable transfer can't pin disk forever (issue #225). Set to 0
          to disable the sweep.
        '';
        type = lib.types.ints.unsigned;
        default = 86400;
      };
    };

    log = {
      level = lib.mkOption {
        default = { };
        description = ''
          Log levels. `default` is the global level; `eval`, `build` and
          `proto` override per component (null inherits from `default`).
        '';

        type = lib.types.submodule {
          options = {
            default = lib.mkOption {
              description = "Default log level for the worker";
              type = logLevelType;
              default = "info";
            };

            eval = lib.mkOption {
              description = "Log level for the evaluator. Null inherits from default";
              type = lib.types.nullOr logLevelType;
              default = null;
            };

            build = lib.mkOption {
              description = "Log level for the builder. Null inherits from default";
              type = lib.types.nullOr logLevelType;
              default = null;
            };

            proto = lib.mkOption {
              description = "Log level for the protocol layer. Null inherits from default";
              type = lib.types.nullOr logLevelType;
              default = null;
            };
          };
        };
      };

      burstBytesPerMin = lib.mkOption {
        description = ''
          Burst token bucket: maximum build-log bytes forwarded to the server
          per build in any 1-minute window. On trip the worker stops forwarding
          log output for that build (the build still runs). Default 8 MiB.
        '';
        type = lib.types.int;
        default = 8 * 1024 * 1024;
      };

      sustainedBytesPerHour = lib.mkOption {
        description = ''
          Sustained token bucket: maximum build-log bytes forwarded to the
          server per build in any 1-hour window. Default 64 MiB.
        '';
        type = lib.types.int;
        default = 64 * 1024 * 1024;
      };

      fetchFromStore = lib.mkOption {
        description = ''
          When a derivation is already built in the local store (so the daemon
          produces no fresh log), read nix's stored `.bz2` build log and forward
          it so the UI still shows output.
        '';
        type = lib.types.bool;
        default = true;
      };
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.capabilities.federate -> cfg.discoverable;
        message = "services.gradient.worker: capabilities.federate requires discoverable to be enabled";
      }
      {
        assertion = !(cfg.reverseProxy.nginx.enable && cfg.reverseProxy.caddy.enable);
        message = "You can only use one reverse proxy at a time";
      }
    ];

    systemd = {
      tmpfiles.settings = lib.mkIf (cfg.gcrootsDir != "") {
        "10-gradient".${cfg.gcrootsDir}.d = {
          user = "gradient-worker";
          group = "gradient-worker";
          mode = "0755";
        };
      };

      # Delegate cgroup-v2 controllers so per-build cgroups expose memory.peak / io.stat.
      services.nix-daemon = lib.mkIf cfg.build.metrics {
        serviceConfig.Delegate = true;
      };

      services.gradient-worker = {
        wantedBy = [ "multi-user.target" ];
        after = [ "network.target" ];
        path = [
          cfg.packages.git
          cfg.packages.nix
          cfg.packages.ssh
        ];

        serviceConfig = {
          Type = "notify";
          ExecStart = lib.getExe' cfg.packages.gradient "gradient-worker";
          StateDirectory = "gradient-worker";
          User = "gradient-worker";
          Group = "gradient-worker";
          PrivateTmp = true;
          ProtectHome = true;
          ProtectHostname = true;
          ProtectKernelLogs = true;
          ProtectKernelModules = true;
          ProtectKernelTunables = true;
          ProtectProc = "invisible";
          ProtectSystem = "strict";
          ReadWritePaths = lib.optionals (cfg.gcrootsDir != "") [ cfg.gcrootsDir ];
          Restart = "on-failure";
          RestartSec = 10;
          # SIGTERM drains: the worker finishes its in-flight jobs before it
          # exits, so systemd must outwait the drain budget rather than
          # SIGKILL a build that is about to finish.
          TimeoutStopSec =
            if cfg.drainTimeoutSecs == 0 then
              "infinity"
            else
              cfg.drainTimeoutSecs + 30;
          KillMode = "mixed";
          LimitNOFILE = 65535;
          # Secrets are mlock'd to keep them off swap; without this the lock
          # fails (EPERM) and floods the log on every SSH-key git operation.
          LimitMEMLOCK = "128M";
          RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
          RestrictNamespaces = true;
          RestrictRealtime = true;
          RestrictSUIDSGID = true;
          WorkingDirectory = cfg.baseDir;
          LoadCredential = lib.optionals (cfg.peersFile != null) [
            "gradient_worker_peers:${cfg.peersFile}"
          ];
        };

        environment = {
          NIX_REMOTE = "daemon";
          XDG_CACHE_HOME = "${cfg.baseDir}/www/.cache";
          RUST_LOG = cfg.log.level.default;
          GRADIENT_WORKER_BASE_DIR = cfg.baseDir;
          GRADIENT_WORKER_NIX_BIN = lib.getExe' cfg.packages.nix "nix";
          GRADIENT_WORKER_SSH_BIN = lib.getExe' cfg.packages.ssh "ssh";
          GRADIENT_WORKER_GCROOTS_DIR = cfg.gcrootsDir;
          GRADIENT_WORKER_DRAIN_TIMEOUT_SECS = toString cfg.drainTimeoutSecs;
          GRADIENT_WORKER_DISCOVERABLE = lib.boolToString cfg.discoverable;
          GRADIENT_WORKER_LISTEN_ADDR = cfg.listenAddr;
          GRADIENT_WORKER_PORT = toString cfg.port;
          GRADIENT_WORKER_CAPABILITIES_FEDERATE = lib.boolToString cfg.capabilities.federate;
          GRADIENT_WORKER_CAPABILITIES_FETCH = lib.boolToString cfg.capabilities.fetch;
          GRADIENT_WORKER_CAPABILITIES_EVAL = lib.boolToString cfg.capabilities.eval;
          GRADIENT_WORKER_CAPABILITIES_BUILD = lib.boolToString cfg.capabilities.build;
          GRADIENT_WORKER_SYSTEM_MIN_FREE_RAM_MB = toString cfg.system.minFreeRamMb;
          GRADIENT_WORKER_NIX_DAEMON_MAX_CONNECTIONS = toString cfg.nixDaemon.maxConnections;
          GRADIENT_WORKER_EVAL_MAX_CONCURRENT = toString cfg.eval.maxConcurrent;
          GRADIENT_WORKER_EVAL_WORKERS = toString cfg.eval.workers;
          GRADIENT_WORKER_EVAL_MAX_RSS = toString cfg.eval.maxRss;
          GRADIENT_WORKER_EVAL_METRICS = lib.boolToString cfg.eval.metrics;
          GRADIENT_WORKER_EVAL_CACHE_SHARE = lib.boolToString cfg.eval.cache.share;
          GRADIENT_WORKER_BUILD_MAX_CONCURRENT = toString cfg.build.maxConcurrent;
          GRADIENT_WORKER_BUILD_METRICS = lib.boolToString cfg.build.metrics;
          GRADIENT_WORKER_BUILD_CGROUP_ROOT = cfg.build.cgroupRoot;
          GRADIENT_WORKER_NAR_MAX_CONCURRENT_UPLOADS = toString cfg.nar.maxConcurrentUploads;
          GRADIENT_WORKER_NAR_PARTIAL_TTL_SECS = toString cfg.nar.partialTtlSecs;
          GRADIENT_WORKER_LOG_LEVEL_DEFAULT = cfg.log.level.default;
          GRADIENT_WORKER_LOG_BURST_BYTES_PER_MIN = toString cfg.log.burstBytesPerMin;
          GRADIENT_WORKER_LOG_SUSTAINED_BYTES_PER_HOUR = toString cfg.log.sustainedBytesPerHour;
          GRADIENT_WORKER_LOG_FETCH_FROM_STORE = lib.boolToString cfg.log.fetchFromStore;
        } // lib.optionalAttrs (cfg.serverUrl != null) {
          GRADIENT_WORKER_SERVER_URL = cfg.serverUrl;
        } // lib.optionalAttrs (cfg.peersFile != null) {
          GRADIENT_WORKER_PEERS_FILE = "%d/gradient_worker_peers";
        } // lib.optionalAttrs (cfg.id != null) {
          GRADIENT_WORKER_ID = cfg.id;
        } // lib.optionalAttrs (cfg.system.architectures != [ ]) {
          GRADIENT_WORKER_SYSTEM_ARCHITECTURES = lib.concatStringsSep "," cfg.system.architectures;
        } // lib.optionalAttrs (cfg.system.features != [ ]) {
          GRADIENT_WORKER_SYSTEM_FEATURES = lib.concatStringsSep "," cfg.system.features;
        } // lib.optionalAttrs (cfg.system.cpuCoreScore != null) {
          GRADIENT_WORKER_SYSTEM_CPU_CORE_SCORE = toString cfg.system.cpuCoreScore;
        } // lib.optionalAttrs (cfg.build.maxCores != null) {
          GRADIENT_WORKER_BUILD_MAX_CORES = toString cfg.build.maxCores;
        } // lib.optionalAttrs (cfg.eval.cache.dir != null) {
          GRADIENT_WORKER_EVAL_CACHE_DIR = cfg.eval.cache.dir;
        } // lib.optionalAttrs (cfg.eval.forkWorkers != null) {
          GRADIENT_WORKER_EVAL_FORK_WORKERS = toString cfg.eval.forkWorkers;
        } // lib.optionalAttrs (cfg.log.level.eval != null) {
          GRADIENT_WORKER_LOG_LEVEL_EVAL = cfg.log.level.eval;
        } // lib.optionalAttrs (cfg.log.level.build != null) {
          GRADIENT_WORKER_LOG_LEVEL_BUILD = cfg.log.level.build;
        } // lib.optionalAttrs (cfg.log.level.proto != null) {
          GRADIENT_WORKER_LOG_LEVEL_PROTO = cfg.log.level.proto;
        };
      };
    };

    nix.package = lib.mkIf cfg.build.metrics (lib.mkDefault cfg.packages.nix);

    nix.settings = {
      trusted-users = [ "gradient-worker" ];
      use-cgroups = lib.mkIf cfg.build.metrics true;
      experimental-features = [
        "nix-command"
        "flakes"
        "ca-derivations"
      ] ++ lib.optional cfg.build.metrics "cgroups";
    };

    services = {
      nginx = lib.mkIf cfg.reverseProxy.nginx.enable {
        enable = true;
        virtualHosts."${cfg.domain}" = {
          enableACME = cfg.useTls;
          forceSSL = cfg.useTls;
          locations."/proto" = {
            proxyPass = "http://${cfg.listenAddr}:${toString cfg.port}";
            proxyWebsockets = true;
            # Matches the server module: the default relay buffer is a single
            # page, which shreds a 4 MiB NAR chunk into hundreds of reads.
            extraConfig = ''
              proxy_buffer_size 256k;
              proxy_buffers 4 256k;
              proxy_connect_timeout 90d;
              proxy_send_timeout 90d;
              proxy_read_timeout 90d;
            '';
          };
        };
      };
      caddy = lib.mkIf cfg.reverseProxy.caddy.enable {
        enable = true;
        virtualHosts."${if cfg.useTls then "" else "http://"}${cfg.domain}" = {
          inherit (cfg.reverseProxy.caddy) useACMEHost;
          # Caddy tunnels the upgraded /proto connection with no intermediate
          # buffer to size, so it needs no counterpart to the nginx tuning.
          extraConfig = ''
            reverse_proxy http://${cfg.listenAddr}:${toString cfg.port}
          '';
        };
      };
    };

    users = {
      groups.gradient-worker = { };
      users.gradient-worker = {
        description = "Gradient Worker user";
        isSystemUser = true;
        group = "gradient-worker";
      };
    };
  };
}
