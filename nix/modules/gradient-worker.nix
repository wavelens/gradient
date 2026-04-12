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
    discoverable = lib.mkEnableOption "listen for incoming connections from servers";
    domain = lib.mkOption {
      description = "Domain under which the worker's nginx vhost is served. Only used when configureNginx is enabled";
      type = lib.types.str;
      default = "";
      example = "worker.example.com";
    };

    serverUrl = lib.mkOption {
      description = "WebSocket URL of the Gradient server protocol endpoint";
      type = lib.types.str;
      default = "";
      example = "wss://worker.example.com/proto";
    };

    peersFile = lib.mkOption {
      description = ''
        Path to a file containing the peer-to-token authentication string for
        challenge-response auth with the Gradient server.

        Format: <literal>peer_id1:token1,peer_id2:token2</literal>
        (comma-separated; each peer is an org UUID paired with the token
        registered via <literal>services.gradient.state.workers</literal>).

        When null (default), the worker connects in open/discoverable mode —
        the server accepts the connection without token validation. Suitable
        for local co-located workers.
      '';
      type = lib.types.nullOr lib.types.path;
      default = null;
    };

    port = lib.mkOption {
      description = "Port for the worker's listener";
      type = lib.types.port;
      default = 3100;
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
        default = 1;
      };

      maxEvaluationsPerWorker = lib.mkOption {
        description = ''
          Recycle an eval-worker subprocess after it has served this many
          list/resolve calls. Set to 0 to disable recycling.
        '';
        type = lib.types.ints.unsigned;
        default = 20;
      };

      maxProtoConnections = lib.mkOption {
        description = "Maximum number of simultaneous proto WebSocket connections";
        type = lib.types.ints.positive;
        default = 16;
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

    systemd.services.gradient-worker = {
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];

      serviceConfig = {
        ExecStart = "${lib.getBin cfg.package}/worker";
        User = "gradient-worker";
        Group = "gradient-worker";
        Restart = "on-failure";
        RestartSec = 10;
        PrivateTmp = true;
        ProtectHome = true;
        ProtectSystem = "strict";
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        StateDirectory = "gradient-worker";
        LoadCredential = lib.optionals (cfg.peersFile != null) [
          "gradient_worker_peers:${cfg.peersFile}"
        ];
      };

      environment = {
        GRADIENT_WORKER_SERVER_URL = cfg.serverUrl;
        GRADIENT_WORKER_DATA_DIR   = "%S/gradient-worker";
      } // lib.optionalAttrs (cfg.peersFile != null) {
        GRADIENT_WORKER_PEERS_FILE = "%d/gradient_worker_peers";
      } // {
        GRADIENT_WORKER_DISCOVERABLE                = lib.boolToString cfg.discoverable;
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

    services.nginx = lib.mkIf cfg.configureNginx {
      enable = true;
      virtualHosts."${cfg.domain}" = {
        locations."/proto" = {
          proxyPass = "http://127.0.0.1:${toString cfg.port}";
          proxyWebsockets = true;
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
