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
        description = "Nix package to use for evaluation and fetching. The `nix` binary from this package is passed to the worker as `GRADIENT_BINPATH_NIX`. Defaults to the gradient nix fork so the shelled-out `nix` matches the worker's embedded fork evaluator.";
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
    discoverable = lib.mkEnableOption "accept incoming connections on /proto";
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

    workerId = lib.mkOption {
      description = ''
        Override the worker's persistent UUID. When set, this value is used as
        the worker identity instead of the UUID auto-generated and stored in
        <literal>$StateDirectory/worker-id</literal> on first start.

        Useful for declarative deployments where the worker UUID must be known
        ahead of time (e.g. to pre-register it in <literal>state.workers</literal>).
        Must be a valid UUID.

        When null (default) the worker reads or generates its ID from the state
        directory as usual.
      '';
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "550e8400-e29b-41d4-a716-446655440001";
    };

    peersFile = lib.mkOption {
      description = ''
        Path to a file containing peer-to-token pairs for challenge-response
        auth with the Gradient server, one entry per line:

        <literal>
        # one peer_id:token per line; lines starting with # are ignored
        &lt;uuid&gt;:&lt;token&gt;
        *:&lt;token&gt;
        </literal>

        The special peer ID <literal>*</literal> matches any UUID the server
        challenges, so a single token works for any org. Each token must be
        a 48-byte random secret (e.g. <literal>openssl rand -base64 48</literal>)
        and is registered via <literal>POST /api/v1/orgs/{org}/workers</literal>.

        In a fully declarative deployment, pin the org UUID with
        <literal>services.gradient.state.organizations.&lt;name&gt;.id</literal>
        on the server and reference the same value here, so the peer id is
        known ahead of the first server start.

        When null (default), the worker connects in open/discoverable mode -
        the server accepts the connection without token validation. Suitable
        for local co-located workers.
      '';
      type = lib.types.nullOr lib.types.path;
      default = null;
    };

    capabilities = {
      federate = lib.mkEnableOption "federate capability - relay work and NAR traffic between workers and servers (requires discoverable)";
      fetch = lib.mkEnableOption "fetch capability - prefetch flake inputs and sources" // { default = true; };
      eval  = lib.mkEnableOption "eval capability - run Nix flake evaluations" // { default = true; };
      build = lib.mkEnableOption "build capability - execute Nix store builds" // { default = true; };
    };

    settings = {
      architectures = lib.mkOption {
        description = "Nix system strings this worker can build for";
        type = lib.types.listOf lib.types.str;
        default = [ pkgs.stdenv.hostPlatform.system ] ++ lib.optional (pkgs.stdenv.hostPlatform.system == "x86_64-linux") "i686-linux";
        defaultText = lib.literalExpression ''[ pkgs.stdenv.hostPlatform.system ] ++ lib.optional (pkgs.stdenv.hostPlatform.system == "x86_64-linux") "i686-linux"'';
        example = [ "x86_64-linux" "aarch64-linux" ];
      };

      systemFeatures = lib.mkOption {
        description = "Nix system features this worker advertises";
        type = lib.types.listOf lib.types.str;
        default = lib.lists.uniqueStrings config.nix.settings.system-features;
        defaultText = lib.literalExpression "lib.lists.uniqueStrings config.nix.settings.system-features";
        example = [ "nixos-test" "benchmark" "big-parallel" ];
      };

      cpuCoreScore = lib.mkOption {
        description = "Override the advertised single-core speed score (higher is faster). When null, the worker benchmarks the host at startup.";
        type = lib.types.nullOr lib.types.ints.positive;
        default = null;
      };

      maxConcurrentEvaluations = lib.mkOption {
        description = "Maximum number of concurrent evaluations";
        type = lib.types.ints.positive;
        default = 1;
      };

      maxConcurrentBuilds = lib.mkOption {
        description = "Maximum number of concurrent builds";
        type = lib.types.ints.positive;
        default = 8;
      };

      maxNixdaemonConnections = lib.mkOption {
        description = ''
          Maximum number of simultaneous local Nix daemon connections in
          the connection pool. Should comfortably fit
          `maxConcurrentBuilds * 8` (parallel NAR imports per build) plus
          headroom for path-presence checks and build dispatch.
        '';
        type = lib.types.ints.positive;
        default = 32;
      };

      narPartialTtlSecs = lib.mkOption {
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

      evalWorkers = lib.mkOption {
        description = "Number of Nix evaluator subprocesses";
        type = lib.types.ints.positive;
        default = 8;
      };

      evalForkWorkers = lib.mkOption {
        description = "Number of parallel eval subprocesses in the pool (the eval concurrency). When null, the worker auto-sizes to the host core count (capped). Each worker may hold up to maxEvalRss of resident memory.";
        type = lib.types.nullOr lib.types.ints.positive;
        default = null;
      };

      maxEvalRss = lib.mkOption {
        description = "Safety cap on an eval subprocess's resident memory: once its RSS exceeds this many bytes it is recycled (parent-side). Keep it above a typical eval's heap so warm workers are not recycled mid-evaluation.";
        type = lib.types.ints.positive;
        default = 8589934592;
      };

      evalCacheDir = lib.mkOption {
        description = "Eval-cache directory exported to eval workers as NIX_CACHE_HOME. When null, resolves to {baseDir}/eval-cache.";
        type = lib.types.nullOr lib.types.str;
        default = null;
      };

      evalCacheShare = lib.mkOption {
        description = "Enable fleet eval-cache sharing (pull/push of <fingerprint>.sqlite blobs across workers).";
        type = lib.types.bool;
        default = true;
      };

      evalMetricsEnabled = lib.mkOption {
        description = "Capture per-evaluation Nix metrics (thunks, heap, peak RSS, per-entry-point hotspots, flake graph). When false, eval-workers skip the stats read (zero overhead).";
        type = lib.types.bool;
        default = true;
      };

      maxProtoConnections = lib.mkOption {
        description = "Maximum number of simultaneous proto WebSocket connections";
        type = lib.types.ints.positive;
        default = 1;
      };

      gcrootsDir = lib.mkOption {
        description = ''
          Directory under which the worker writes one indirect GC root
          symlink per active build (drv + outputs). Pins inputs and
          just-built outputs through the local nix-daemon so a concurrent
          <literal>nix-collect-garbage</literal> cannot delete them
          mid-build. A systemd-tmpfiles rule creates the directory under
          the worker user and adds it to the unit's
          <literal>ReadWritePaths</literal>.

          Set to an empty string to disable GC root pinning (the worker
          still builds, but a concurrent GC may race the build).
        '';
        type = lib.types.str;
        default = "/nix/var/nix/gcroots/gradient";
      };

      buildMetrics = lib.mkOption {
        description = ''
          Capture per-build resource metrics (peak RAM, CPU time, disk I/O).
          Enables Nix's experimental <literal>cgroups</literal> feature and
          <literal>use-cgroups</literal> on the daemon. CPU time comes from the
          daemon build result; peak RAM and disk I/O are sampled live from the
          build's cgroup (located via <literal>buildCgroupStateDir</literal>) -
          reliable at build concurrency 1, best-effort under concurrency.
          Wall-clock build time is always reported.
        '';
        type = lib.types.bool;
        default = false;
      };

      buildCgroupRoot = lib.mkOption {
        description = ''
          Cgroup-v2 mount root searched for per-build cgroups when
          <literal>buildMetrics</literal> is enabled.
        '';
        type = lib.types.str;
        default = "/sys/fs/cgroup";
      };

      buildCgroupStateDir = lib.mkOption {
        description = ''
          Nix's <literal>&lt;nix-state-dir&gt;/cgroups</literal> directory, where
          the daemon records each build's cgroup path (`<uid>` files). The worker
          reads the newest entry to locate a running build's cgroup for metrics.
          Granted read access via <literal>ReadOnlyPaths</literal>.
        '';
        type = lib.types.str;
        default = "/nix/var/nix/cgroups";
      };

      logBurstBytesPerMin = lib.mkOption {
        description = ''
          Burst token bucket: maximum build-log bytes forwarded to the server
          per build in any 1-minute window. On trip the worker stops forwarding
          log output for that build (the build still runs). Default 8 MiB.
        '';
        type = lib.types.int;
        default = 8 * 1024 * 1024;
      };

      logSustainedBytesPerHour = lib.mkOption {
        description = ''
          Sustained token bucket: maximum build-log bytes forwarded to the
          server per build in any 1-hour window. Default 64 MiB.
        '';
        type = lib.types.int;
        default = 64 * 1024 * 1024;
      };

      logFetchFromStore = lib.mkOption {
        description = ''
          When a derivation is already built in the local store (so the daemon
          produces no fresh log), read nix's stored <literal>.bz2</literal> build
          log and forward it so the UI still shows output.
        '';
        type = lib.types.bool;
        default = true;
      };

      logLevel = lib.mkOption {
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
      tmpfiles.settings = lib.mkIf (cfg.settings.gcrootsDir != "") {
        "10-gradient".${cfg.settings.gcrootsDir}.d = {
          user = "gradient-worker";
          group = "gradient-worker";
          mode = "0755";
        };
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
          ReadWritePaths = lib.optionals (cfg.settings.gcrootsDir != "") [ cfg.settings.gcrootsDir ];
          # Build metrics read the daemon's cgroup-path map and the cgroup-v2
          # stat files. ProtectSystem=strict already leaves /sys and /nix
          # readable; this makes the cgroup map explicitly available (the dir may
          # not exist until the first cgroup build, hence the `-` prefix).
          ReadOnlyPaths = lib.optionals cfg.settings.buildMetrics [ "-${cfg.settings.buildCgroupStateDir}" ];
          Restart = "on-failure";
          RestartSec = 10;
          KillMode = "mixed";
          LimitNOFILE = 65535;
          # Secrets are mlock'd to keep them off swap; without this the lock
          # fails (EPERM) and floods the log on every SSH-key git operation.
          LimitMEMLOCK = "64M";
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
          GRADIENT_WORKER_DATA_DIR   = cfg.baseDir;
          GRADIENT_BINPATH_NIX       = lib.getExe' cfg.packages.nix "nix";
          GRADIENT_BINPATH_SSH       = lib.getExe' cfg.packages.ssh "ssh";
        } // lib.optionalAttrs (cfg.serverUrl != null) {
          GRADIENT_WORKER_SERVER_URL = cfg.serverUrl;
        } // lib.optionalAttrs (cfg.peersFile != null) {
          GRADIENT_WORKER_PEERS_FILE = "%d/gradient_worker_peers";
        } // lib.optionalAttrs (cfg.workerId != null) {
          GRADIENT_WORKER_ID = cfg.workerId;
        } // {
          GRADIENT_WORKER_DISCOVERABLE                = lib.boolToString cfg.discoverable;
          GRADIENT_WORKER_LISTEN_ADDR                 = cfg.listenAddr;
          GRADIENT_WORKER_PORT                        = toString cfg.port;
          GRADIENT_MAX_CONCURRENT_EVALUATIONS         = toString cfg.settings.maxConcurrentEvaluations;
          GRADIENT_MAX_CONCURRENT_BUILDS              = toString cfg.settings.maxConcurrentBuilds;
          GRADIENT_MAX_NIXDAEMON_CONNECTIONS          = toString cfg.settings.maxNixdaemonConnections;
          GRADIENT_NAR_PARTIAL_TTL_SECS               = toString cfg.settings.narPartialTtlSecs;
          GRADIENT_WORKER_EVAL_WORKERS                = toString cfg.settings.evalWorkers;
          GRADIENT_MAX_EVAL_RSS                       = toString cfg.settings.maxEvalRss;
          GRADIENT_EVAL_CACHE_SHARE                   = lib.boolToString cfg.settings.evalCacheShare;
          GRADIENT_EVAL_METRICS_ENABLED               = lib.boolToString cfg.settings.evalMetricsEnabled;
          GRADIENT_MAX_PROTO_CONNECTIONS              = toString cfg.settings.maxProtoConnections;
          GRADIENT_WORKER_GCROOTS_DIR                 = cfg.settings.gcrootsDir;
          GRADIENT_WORKER_BUILD_METRICS               = lib.boolToString cfg.settings.buildMetrics;
          GRADIENT_WORKER_BUILD_CGROUP_ROOT           = cfg.settings.buildCgroupRoot;
          GRADIENT_WORKER_BUILD_CGROUP_STATE_DIR      = cfg.settings.buildCgroupStateDir;
          GRADIENT_LOG_BURST_BYTES_PER_MIN            = toString cfg.settings.logBurstBytesPerMin;
          GRADIENT_LOG_SUSTAINED_BYTES_PER_HOUR       = toString cfg.settings.logSustainedBytesPerHour;
          GRADIENT_LOG_FETCH_FROM_STORE               = lib.boolToString cfg.settings.logFetchFromStore;
        } // lib.optionalAttrs (cfg.settings.architectures != []) {
          GRADIENT_WORKER_ARCHITECTURES = lib.concatStringsSep "," cfg.settings.architectures;
        } // lib.optionalAttrs (cfg.settings.systemFeatures != []) {
          GRADIENT_WORKER_SYSTEM_FEATURES = lib.concatStringsSep "," cfg.settings.systemFeatures;
        } // lib.optionalAttrs (cfg.settings.cpuCoreScore != null) {
          GRADIENT_WORKER_CPU_CORE_SCORE = toString cfg.settings.cpuCoreScore;
        } // lib.optionalAttrs (cfg.settings.evalCacheDir != null) {
          GRADIENT_EVAL_CACHE_DIR = cfg.settings.evalCacheDir;
        } // lib.optionalAttrs (cfg.settings.evalForkWorkers != null) {
          GRADIENT_EVAL_FORK_WORKERS = toString cfg.settings.evalForkWorkers;
        } // {
          GRADIENT_WORKER_CAPABILITY_FEDERATE         = lib.boolToString cfg.capabilities.federate;
          GRADIENT_WORKER_CAPABILITY_FETCH            = lib.boolToString cfg.capabilities.fetch;
          GRADIENT_WORKER_CAPABILITY_EVAL             = lib.boolToString cfg.capabilities.eval;
          GRADIENT_WORKER_CAPABILITY_BUILD            = lib.boolToString cfg.capabilities.build;
          GRADIENT_LOG_LEVEL                          = cfg.settings.logLevel.default;
          RUST_LOG                                    = cfg.settings.logLevel.default;
        } // lib.optionalAttrs (cfg.settings.logLevel.eval != null) {
          GRADIENT_EVAL_LOG_LEVEL = cfg.settings.logLevel.eval;
        } // lib.optionalAttrs (cfg.settings.logLevel.build != null) {
          GRADIENT_BUILD_LOG_LEVEL = cfg.settings.logLevel.build;
        } // lib.optionalAttrs (cfg.settings.logLevel.proto != null) {
          GRADIENT_PROTO_LOG_LEVEL = cfg.settings.logLevel.proto;
        };
      };
    };

    nix.settings = {
      trusted-users = [ "gradient-worker" ];
      use-cgroups = lib.mkIf cfg.settings.buildMetrics true;
      experimental-features = [
        "nix-command"
        "flakes"
        "ca-derivations"
      ] ++ lib.optional cfg.settings.buildMetrics "cgroups";
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
