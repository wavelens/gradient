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
    package = lib.mkPackageOption pkgs "gradient" { };
    configureNginx = lib.mkEnableOption "Nginx reverse proxy for the worker listener";
    useTls = lib.mkEnableOption "TLS" // { default = true; };
    discoverable = lib.mkEnableOption "listen for incoming connections from servers";
    domain = lib.mkOption {
      description = "Domain under which the worker's nginx vhost is served. Only used when configureNginx is enabled";
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

        When null (default), the worker connects in open/discoverable mode —
        the server accepts the connection without token validation. Suitable
        for local co-located workers.
      '';
      type = lib.types.nullOr lib.types.path;
      default = null;
    };

    capabilities = {
      federate = lib.mkEnableOption "federate capability — relay work and NAR traffic between workers and servers (requires discoverable)";
      fetch = lib.mkEnableOption "fetch capability — prefetch flake inputs and sources";
      eval  = lib.mkEnableOption "eval capability — run Nix flake evaluations";
      build = lib.mkEnableOption "build capability — execute Nix store builds";
      sign  = lib.mkEnableOption "sign capability — sign and upload store paths";
    };

    settings = {
      maxConcurrentEvaluations = lib.mkOption {
        description = "Maximum number of concurrent evaluations";
        type = lib.types.ints.positive;
        default = 1;
      };

      maxConcurrentBuilds = lib.mkOption {
        description = "Maximum number of concurrent builds";
        type = lib.types.ints.positive;
        default = 100;
      };

      maxNixdaemonConnections = lib.mkOption {
        description = "Maximum number of simultaneous local Nix daemon connections in the connection pool";
        type = lib.types.ints.positive;
        default = 24;
      };

      evalWorkers = lib.mkOption {
        description = "Number of Nix evaluator subprocesses";
        type = lib.types.ints.positive;
        default = 4;
      };

      maxEvaluationsPerWorker = lib.mkOption {
        description = ''
          Recycle an eval-worker subprocess after it has served this many
          list/resolve calls. Set to 0 to disable recycling.
        '';
        type = lib.types.ints.unsigned;
        default = 1;
      };

      maxProtoConnections = lib.mkOption {
        description = "Maximum number of simultaneous proto WebSocket connections";
        type = lib.types.ints.positive;
        default = 1;
      };

      evalClosureParallelism = lib.mkOption {
        description = "Number of top-level derivations whose dependency closure is walked in parallel";
        type = lib.types.ints.positive;
        default = 8;
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
    ];

    systemd = {
      tmpfiles.settings."10-gradient"."/nix/var/nix/gcroots/gradient".d = {
        user = "gradient-worker";
        group = "gradient-worker";
        mode = "0755";
      };

      services.gradient-worker = {
        wantedBy = [ "multi-user.target" ];
        after = [ "network.target" ];
        serviceConfig = {
          ExecStart = lib.getExe' cfg.package "gradient-worker";
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
          ReadWritePaths = [ "/nix/var/nix/gcroots/gradient" ];
          Restart = "on-failure";
          RestartSec = 10;
          LimitNOFILE = 65535;
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
          GRADIENT_WORKER_EVAL_WORKERS                = toString cfg.settings.evalWorkers;
          GRADIENT_MAX_EVALUATIONS_PER_WORKER         = toString cfg.settings.maxEvaluationsPerWorker;
          GRADIENT_EVAL_CLOSURE_PARALLELISM           = toString cfg.settings.evalClosureParallelism;
          GRADIENT_MAX_PROTO_CONNECTIONS              = toString cfg.settings.maxProtoConnections;
          GRADIENT_WORKER_CAPABILITY_FEDERATE         = lib.boolToString cfg.capabilities.federate;
          GRADIENT_WORKER_CAPABILITY_FETCH            = lib.boolToString cfg.capabilities.fetch;
          GRADIENT_WORKER_CAPABILITY_EVAL             = lib.boolToString cfg.capabilities.eval;
          GRADIENT_WORKER_CAPABILITY_BUILD            = lib.boolToString cfg.capabilities.build;
          GRADIENT_WORKER_CAPABILITY_SIGN             = lib.boolToString cfg.capabilities.sign;
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
      experimental-features = [
        "nix-command"
        "flakes"
        "ca-derivations"
      ];
    };

    services.nginx = lib.mkIf cfg.configureNginx {
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
