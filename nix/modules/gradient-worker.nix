/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, config, ... }: let
  logLevelType = lib.types.enum [ "trace" "debug" "info" "warn" "error" ];

  workerOpts = { name, ... }: {
    options = {
      enable = lib.mkEnableOption "this Gradient worker instance";
      package = lib.mkPackageOption pkgs "gradient" { };
      configureNginx = lib.mkEnableOption "Nginx";
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
        example = "wss://gradient.example.com/proto";
      };

      tokenFile = lib.mkOption {
        description = "File containing the API key used to authenticate with the server. Not required for cache-only (public) connections";
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
            Recycle an eval-worker subprocess after it has served this
            many list/resolve calls. Nix's Boehm GC never releases
            memory back to the OS, so long-lived workers grow
            monotonically; this cap bounds RSS growth by forcing a
            respawn. Set to 0 to disable recycling.
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
          description = ''
            Number of top-level derivations whose dependency closure is
            walked in parallel during the EvaluatingDerivation phase.
            Each walker issues DB and Nix-store queries concurrently, so
            raising this reduces evaluation latency at the cost of DB
            pool / nix-daemon pressure.
          '';
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
  };

  enabledWorkers = lib.filterAttrs (_: wcfg: wcfg.enable) config.services.gradient.workers;
in {
  options.services.gradient.workers = lib.mkOption {
    description = "Gradient worker instances. Each entry creates a separate systemd service connecting to its configured server.";
    type = lib.types.attrsOf (lib.types.submodule workerOpts);
    default = { };
    example = lib.literalExpression ''
      {
        local = {
          enable = true;
          serverUrl = "ws://127.0.0.1:3000/proto";
          capabilities = { fetch = true; eval = true; build = true; sign = true; };
        };
        remote = {
          enable = true;
          serverUrl = "wss://gradient.example.com/proto";
          tokenFile = "/run/secrets/gradient-remote-token";
          capabilities = { build = true; };
        };
      }
    '';
  };

  config = lib.mkIf (enabledWorkers != { }) {
    assertions = lib.concatLists (lib.mapAttrsToList (name: wcfg: [
      {
        assertion = wcfg.capabilities.federate -> wcfg.discoverable;
        message = "workers.${name}: capabilities.federate requires discoverable to be enabled";
      }
      {
        assertion = (wcfg.capabilities.federate || wcfg.capabilities.fetch || wcfg.capabilities.eval || wcfg.capabilities.build || wcfg.capabilities.sign) -> (wcfg.tokenFile != null);
        message = "workers.${name}: tokenFile is required when any capability other than cache is enabled";
      }
    ]) enabledWorkers);

    systemd.services = lib.mapAttrs' (name: wcfg: lib.nameValuePair "gradient-worker-${name}" {
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];

      serviceConfig = {
        ExecStart = "${lib.getBin wcfg.package}/gradient-worker";
        User = "gradient-worker";
        Group = "gradient-worker";
        Restart = "on-failure";
        RestartSec = 10;
        PrivateTmp = true;
        ProtectHome = true;
        ProtectSystem = "strict";
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        StateDirectory = "gradient-worker-${name}";
        LoadCredential = lib.optionals (wcfg.tokenFile != null) [
          "gradient_worker_token:${wcfg.tokenFile}"
        ];
      };

      environment = {
        GRADIENT_WORKER_SERVER_URL    = wcfg.serverUrl;
      } // lib.optionalAttrs (wcfg.tokenFile != null) {
        GRADIENT_WORKER_TOKEN_FILE    = "%d/gradient_worker_token";
      } // {
        GRADIENT_WORKER_DISCOVERABLE = lib.boolToString wcfg.discoverable;
        GRADIENT_MAX_CONCURRENT_EVALUATIONS = toString wcfg.settings.maxConcurrentEvaluations;
        GRADIENT_MAX_CONCURRENT_BUILDS = toString wcfg.settings.maxConcurrentBuilds;
        GRADIENT_MAX_NIXDAEMON_CONNECTIONS = toString wcfg.settings.maxNixdaemonConnections;
        GRADIENT_WORKER_EVAL_WORKERS  = toString wcfg.settings.evalWorkers;
        GRADIENT_MAX_EVALUATIONS_PER_WORKER = toString wcfg.settings.maxEvaluationsPerWorker;
        GRADIENT_EVAL_CLOSURE_PARALLELISM = toString wcfg.settings.evalClosureParallelism;
        GRADIENT_MAX_PROTO_CONNECTIONS = toString wcfg.settings.maxProtoConnections;
        GRADIENT_WORKER_CAPABILITY_FEDERATE = lib.boolToString wcfg.capabilities.federate;
        GRADIENT_WORKER_CAPABILITY_FETCH = lib.boolToString wcfg.capabilities.fetch;
        GRADIENT_WORKER_CAPABILITY_EVAL  = lib.boolToString wcfg.capabilities.eval;
        GRADIENT_WORKER_CAPABILITY_BUILD = lib.boolToString wcfg.capabilities.build;
        GRADIENT_WORKER_CAPABILITY_SIGN  = lib.boolToString wcfg.capabilities.sign;
        GRADIENT_LOG_LEVEL = wcfg.settings.logLevel.default;
        RUST_LOG = wcfg.settings.logLevel.default;
      } // lib.optionalAttrs (wcfg.settings.logLevel.eval != null) {
        GRADIENT_EVAL_LOG_LEVEL = wcfg.settings.logLevel.eval;
      } // lib.optionalAttrs (wcfg.settings.logLevel.build != null) {
        GRADIENT_BUILD_LOG_LEVEL = wcfg.settings.logLevel.build;
      } // lib.optionalAttrs (wcfg.settings.logLevel.proto != null) {
        GRADIENT_PROTO_LOG_LEVEL = wcfg.settings.logLevel.proto;
      };
    }) enabledWorkers;

    services.nginx = lib.mkMerge (lib.mapAttrsToList (_: wcfg:
      lib.mkIf wcfg.configureNginx {
        enable = true;
        virtualHosts."${wcfg.domain}" = {
          locations."/proto" = {
            proxyPass = "http://127.0.0.1:${toString wcfg.port}";
            proxyWebsockets = true;
          };
        };
      }
    ) enabledWorkers);

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
