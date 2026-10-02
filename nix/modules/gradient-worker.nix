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
    enable = lib.mkEnableOption "the Gradient worker";

    packages = {
      gradient = lib.mkPackageOption pkgs "gradient" { };
      nix = lib.mkOption {
        type = lib.types.package;
        default = pkgs.gradient-nix;
        defaultText = lib.literalExpression "pkgs.gradient-nix";
        description = ''
          Nix package whose {command}`nix` the worker is running for evaluation and fetching. The
          default is Gradient's Nix fork, matching the worker's embedded evaluator.
        '';
      };

      git = lib.mkOption {
        type = lib.types.package;
        default = config.programs.git.package;
        defaultText = lib.literalExpression "config.programs.git.package";
        description = "Git package available to the worker for cloning repositories.";
      };

      ssh = lib.mkOption {
        type = lib.types.package;
        default = config.programs.ssh.package;
        defaultText = lib.literalExpression "config.programs.ssh.package";
        description = ''
          OpenSSH package used as {env}`GIT_SSH_COMMAND` to fetch private flake inputs.
        '';
      };
    };

    reverseProxy = {
      nginx.enable = lib.mkEnableOption "an nginx virtual host for the worker listener";
      caddy = {
        enable = lib.mkEnableOption "a Caddy virtual host for the worker listener";
        useACMEHost = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = ''
            Host of an existing ACME certificate to use, passed to
            {option}`services.caddy.virtualHosts.<name>.useACMEHost`. No certificate is requested
            for it. `null` is leaving certificate management to Caddy.
          '';
        };

        extraConfig = lib.mkOption {
          type = lib.types.lines;
          default = "";
          description = ''
            Additional lines appended to {option}`services.caddy.virtualHosts.<name>.extraConfig`
            after the reverse proxy setup.
          '';
        };
      };
    };

    useTls = lib.mkEnableOption "TLS" // { default = true; };

    discoverable = lib.mkEnableOption "incoming server connections on `/proto`";

    domain = lib.mkOption {
      type = lib.types.str;
      default = "";
      example = "worker.example.com";
      description = ''
        Domain of the worker's reverse proxy virtual host. It is only used with a reverse proxy
        enabled.
      '';
    };

    serverUrl = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "wss://gradient.example.com/proto";
      description = ''
        WebSocket URL of the Gradient server's `/proto` endpoint. `null` is leaving the worker
        waiting for the server to connect.
      '';
    };

    baseDir = lib.mkOption {
      type = lib.types.path;
      default = "/var/lib/gradient-worker";
      description = "Directory holding the worker's state.";
    };

    listenAddr = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1";
      description = "IP address the worker is listening on for incoming server connections.";
    };

    port = lib.mkOption {
      type = lib.types.port;
      default = 3100;
      description = "Port the worker is listening on for incoming server connections.";
    };

    id = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "550e8400-e29b-41d4-a716-446655440001";
      description = ''
        Worker UUID. `null` is generating one on first start and storing it in
        {file}`<services.gradient.worker.baseDir>/worker-id`. Set it when the UUID must be known in
        advance, for example to register the worker in {option}`services.gradient.state.workers`.
      '';
    };

    zone = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "fra1";
      description = ''
        Locality label advertised to the scheduler. A cluster job asking for one zone is placing
        every member on workers with the same label. `null` is putting the worker in the zone of
        all unlabelled workers.
      '';
    };

    endpoint = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "10.0.0.7:7000";
      description = ''
        Address at which other members of a cluster job are reaching this worker, passed through
        verbatim in the cluster roster. `null` is advertising none.
      '';
    };

    peersFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        File of peer tokens for challenge-response authentication with the server, one
        `peer_id:token` per line. Lines starting with `#` are ignored.

        ```
        <uuid>:<token>
        *:<token>
        ```

        The peer ID `*` is matching any UUID in the server's challenge. Each token is a 48 byte
        random secret, for example from {command}`openssl rand -base64 48`, registered through
        `POST /api/v1/projects/{project}/workers`. Pin a project UUID with
        {option}`services.gradient.state.projects.<name>.id` to reference it here.

        `null` is connecting in open mode, where the server is accepting the worker without a
        token.
      '';
    };

    acceptedServerTokensFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        File of token hashes a server must present when it is dialing this worker, one
        `peer_id:hash` per line. Lines starting with `#` are ignored. A hash is an argon2 PHC
        string or the lowercase SHA-256 hex digest of the token, for example from
        {command}`printf %s "$TOKEN" | sha256sum`. The peer ID `*` is matching any peer. The file
        is only read with {option}`services.gradient.worker.discoverable`.

        `null` is accepting every server, and the worker is logging a warning at start.
      '';
    };

    drainTimeoutSecs = lib.mkOption {
      type = lib.types.ints.unsigned;
      default = 60;
      description = ''
        Seconds a stop is waiting for running jobs. The worker is no longer accepting work,
        finishing and reporting what is running, then exiting. Jobs still running at the deadline
        are aborted and re-queued. The unit's `TimeoutStopSec` is derived from this value. `0` is
        waiting without limit, and a stuck build is then blocking {command}`systemctl stop` until a
        second signal.
      '';
    };

    gcrootsDir = lib.mkOption {
      type = lib.types.str;
      default = "/nix/var/nix/gcroots/gradient";
      description = ''
        Directory for the indirect GC roots pinning each running build's inputs and outputs against
        a concurrent {command}`nix-collect-garbage`. An empty string is disabling pinning.
      '';
    };

    capabilities = {
      federate = lib.mkEnableOption "forwarding work and NARs between workers and servers (requiring `discoverable`)";
      fetch = lib.mkEnableOption "prefetching flake inputs and sources" // { default = true; };
      eval = lib.mkEnableOption "Nix flake evaluations" // { default = true; };
      build = lib.mkEnableOption "Nix builds" // { default = true; };
    };

    system = {
      architectures = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ pkgs.stdenv.hostPlatform.system ] ++ lib.optional (pkgs.stdenv.hostPlatform.system == "x86_64-linux") "i686-linux";
        defaultText = lib.literalExpression ''[ pkgs.stdenv.hostPlatform.system ] ++ lib.optional (pkgs.stdenv.hostPlatform.system == "x86_64-linux") "i686-linux"'';
        example = [ "x86_64-linux" "aarch64-linux" ];
        description = "Nix system types this worker is building for.";
      };

      features = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        example = [ "nixos-test" "benchmark" "big-parallel" ];
        description = ''
          Nix system features this worker is advertising. An empty list is detecting them at runtime from
          {command}`nix config show system-features`, including CPU-derived `gccarch-*` levels.
        '';
      };

      cpuCoreScore = lib.mkOption {
        type = lib.types.nullOr lib.types.ints.positive;
        default = null;
        description = ''
          Single-core speed score advertised to the scheduler, higher is faster. `null` is
          benchmarking the host at startup.
        '';
      };

      minFreeRamMb = lib.mkOption {
        type = lib.types.ints.unsigned;
        default = 0;
        description = ''
          Free memory in MiB below which the worker is killing the one evaluation subprocess large
          enough to restore the margin. The worker is reporting that evaluation as failed instead of
          letting the host freeze. `0` is using 10% of total memory, clamped to 128 MiB to 1 GiB.
          {option}`services.gradient.worker.eval.maxRss` is still bounding steady-state memory.
        '';
      };
    };

    nixDaemon = {
      maxConnections = lib.mkOption {
        type = lib.types.ints.positive;
        default = cfg.build.maxConcurrent * 9 + 16;
        defaultText = lib.literalExpression "config.services.gradient.worker.build.maxConcurrent * 9 + 16";
        description = ''
          Maximum connections to the local Nix daemon. Each build is holding one for its whole run
          plus up to 8 for parallel NAR imports. The rest is headroom for path checks.
        '';
      };
    };

    eval = {
      maxConcurrent = lib.mkOption {
        type = lib.types.ints.positive;
        default = 1;
        description = "Maximum simultaneous evaluations.";
      };

      forkWorkers = lib.mkOption {
        type = lib.types.nullOr lib.types.ints.positive;
        default = null;
        description = ''
          Evaluation subprocesses in the pool, equal to the evaluation concurrency. `null` is sizing
          the pool to the host's core count, capped at 16. The pool is shrinking further until its
          size times {option}`services.gradient.worker.eval.maxRss` is fitting in 75% of the host's
          RAM.
        '';
      };

      maxRss = lib.mkOption {
        type = lib.types.ints.positive;
        default = 2 * 1024 * 1024 * 1024;
        description = ''
          Memory in bytes above which the worker is recycling an evaluation subprocess after its
          current call. The limit is not hard. A subprocess may exceed the limit during a call. Keep
          it above a typical evaluation's heap to avoid recycling warm subprocesses mid-evaluation.
        '';
      };

      metrics = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Whether to collect per-evaluation Nix statistics (thunks, heap, peak memory, hotspots,
          flake graph). Disabling it is removing their overhead.
        '';
      };

      cache = {
        dir = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = ''
            Eval cache directory, exported to evaluation subprocesses as {env}`NIX_CACHE_HOME`.
            `null` is using {file}`<services.gradient.worker.baseDir>/eval-cache`.
          '';
        };

        share = lib.mkOption {
          type = lib.types.bool;
          default = true;
          description = "Whether to share eval cache blobs with other workers through the server.";
        };
      };
    };

    build = {
      maxConcurrent = lib.mkOption {
        type = lib.types.ints.positive;
        default = 32;
        description = "Maximum simultaneous builds.";
      };

      maxCores = lib.mkOption {
        type = lib.types.nullOr lib.types.ints.positive;
        default = null;
        description = ''
          CPU cores a single build may use, passed as `--cores`. `null` is using all cores.
        '';
      };

      metrics = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = ''
          Whether to record per-build peak memory, CPU time and disk I/O. Enabling it is turning on
          Nix's experimental `cgroups` feature and `use-cgroups` and delegating cgroup controllers to
          {file}`nix-daemon.service`. Peak memory and disk I/O need Gradient's Nix fork on the
          daemon, and {option}`nix.package` is defaulting to its package. Wall-clock time is always
          recorded.
        '';
      };

      cgroupRoot = lib.mkOption {
        type = lib.types.str;
        default = "/sys/fs/cgroup/system.slice/nix-daemon.service";
        description = ''
          Cgroup of the Nix daemon. The daemon is creating each build's cgroup under this cgroup when
          {option}`services.gradient.worker.build.metrics` is enabled.
        '';
      };
    };

    nar = {
      maxConcurrentUploads = lib.mkOption {
        type = lib.types.ints.positive;
        default = 16;
        description = ''
          Upload requests over 1 MiB the worker is keeping open at once, waiting for a server grant
          or transferring. One job is holding at most half. Smaller uploads have a window of 128 of
          their own. The server's upload budget is deciding how many run. This limit is bounding
          worker memory.
        '';
      };

      partialTtlSecs = lib.mkOption {
        type = lib.types.ints.unsigned;
        default = 86400;
        description = ''
          Seconds after its last write before the deletion of an unfinished NAR download under
          {file}`<services.gradient.worker.baseDir>/nar-partial`. `0` is disabling the cleanup.
        '';
      };
    };

    log = {
      level = lib.mkOption {
        type = lib.types.submodule {
          options = {
            default = lib.mkOption {
              type = logLevelType;
              default = "info";
              description = "Default log level.";
            };

            eval = lib.mkOption {
              type = lib.types.nullOr logLevelType;
              default = null;
              description = ''
                Log level of the evaluator. `null` is using
                {option}`services.gradient.worker.log.level.default`.
              '';
            };

            build = lib.mkOption {
              type = lib.types.nullOr logLevelType;
              default = null;
              description = ''
                Log level of the builder. `null` is using
                {option}`services.gradient.worker.log.level.default`.
              '';
            };

            proto = lib.mkOption {
              type = lib.types.nullOr logLevelType;
              default = null;
              description = ''
                Log level of the protocol layer. `null` is using
                {option}`services.gradient.worker.log.level.default`.
              '';
            };
          };
        };
        default = { };
        description = "Log levels per component.";
      };

      burstBytesPerMin = lib.mkOption {
        type = lib.types.int;
        default = 8 * 1024 * 1024;
        description = ''
          Build log bytes forwarded per build within any minute. The worker is no longer forwarding
          a build's log past this limit. The build itself is continuing.
        '';
      };

      sustainedBytesPerHour = lib.mkOption {
        type = lib.types.int;
        default = 64 * 1024 * 1024;
        description = "Build log bytes forwarded per build within any hour.";
      };

      fetchFromStore = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Whether to forward the stored build log of a derivation already built locally and
          therefore producing no new log.
        '';
      };

      traceDir = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "/var/lib/gradient-worker/trace";
        description = ''
          Directory receiving every closed stage span of the worker and its eval subprocesses as
          JSON lines, one file per process. `null` is disabling span tracing.
        '';
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
      tmpfiles.settings."10-gradient" = lib.optionalAttrs (cfg.gcrootsDir != "") {
        ${cfg.gcrootsDir}.d = {
          user = "gradient-worker";
          group = "gradient-worker";
          mode = "0755";
        };
      } // lib.optionalAttrs (cfg.log.traceDir != null) {
        ${cfg.log.traceDir}.d = {
          user = "gradient-worker";
          group = "gradient-worker";
          mode = "0750";
        };
      };

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
          ReadWritePaths = lib.optionals (cfg.gcrootsDir != "") [ cfg.gcrootsDir ]
            ++ lib.optional (cfg.log.traceDir != null) cfg.log.traceDir;
          Restart = "on-failure";
          RestartSec = 10;
          # SIGTERM is draining the worker until its in-flight jobs are finished. systemd must outwait
          # the drain budget instead of killing a build about to finish.
          TimeoutStopSec =
            if cfg.drainTimeoutSecs == 0 then
              "infinity"
            else
              cfg.drainTimeoutSecs + 30;
          KillMode = "mixed";
          LimitNOFILE = 65535;
          # Secrets are mlock'd to keep them off swap. The lock is failing with EPERM below this
          # limit and flooding the log on every SSH-key git operation.
          LimitMEMLOCK = "128M";
          RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
          RestrictNamespaces = true;
          RestrictRealtime = true;
          RestrictSUIDSGID = true;
          WorkingDirectory = cfg.baseDir;
          LoadCredential = lib.optionals (cfg.peersFile != null) [
            "gradient_worker_peers:${cfg.peersFile}"
          ] ++ lib.optionals (cfg.acceptedServerTokensFile != null) [
            "gradient_worker_accepted_server_tokens:${cfg.acceptedServerTokensFile}"
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
        } // lib.optionalAttrs (cfg.acceptedServerTokensFile != null) {
          GRADIENT_WORKER_ACCEPTED_SERVER_TOKENS_FILE = "%d/gradient_worker_accepted_server_tokens";
        } // lib.optionalAttrs (cfg.id != null) {
          GRADIENT_WORKER_ID = cfg.id;
        } // lib.optionalAttrs (cfg.zone != null) {
          GRADIENT_WORKER_ZONE = cfg.zone;
        } // lib.optionalAttrs (cfg.endpoint != null) {
          GRADIENT_WORKER_ENDPOINT = cfg.endpoint;
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
        } // lib.optionalAttrs (cfg.log.traceDir != null) {
          GRADIENT_WORKER_LOG_TRACE_DIR = cfg.log.traceDir;
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
