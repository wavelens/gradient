/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, cfg }: let
  workerOptionPathsOnlyLinuxModuleDeclares = [
    [ "exec" "image" ]
    [ "exec" "devices" ]
    [ "exec" "mounts" ]
    [ "exec" "allow" "ports" ]
    [ "exec" "cloudHypervisorBin" ]
    [ "exec" "virtiofsdBin" ]
    [ "build" "cgroup" ]
  ];

  workerOptions = import ../options.nix {
    inherit lib;
    specs = builtins.fromJSON (builtins.readFile ../../../backend/gradient-worker/options.json);
    subGroups = { eval = [ "cache" ]; log = [ "level" ]; exec = [ "allow" "defaults" "sops" ]; };
    groupSwitches.exec = [ "capabilities" "exec" ];

    examples = {
      endpoint = "10.0.0.7:7000";
      id = "550e8400-e29b-41d4-a716-446655440001";
      serverUrl = "wss://gradient.example.com/proto";
      zone = "fra1";
      log.traceDir = "/var/lib/gradient-worker/trace";
      exec.sops.files = [ "/etc/gradient/worker-secrets.yaml" ];
    };

    withoutEnvironment = [ [ "peers" ] [ "sshBin" ] [ "evalSubprocess" ] [ "evalDriver" ] [ "guestSupervisor" ] ]
      ++ workerOptionPathsOnlyLinuxModuleDeclares;

    writtenByHand = workerOptionPathsOnlyLinuxModuleDeclares ++ [
      [ "peers" ]
      [ "sshBin" ]
      [ "evalSubprocess" ]
      [ "evalDriver" ]
      [ "guestSupervisor" ]
      [ "system" "architectures" ]
      [ "system" "features" ]
      [ "nixDaemon" "maxConnections" ]
      [ "nar" "chunkBytes" ]
      [ "log" "level" "default" ]
      [ "log" "level" "eval" ]
      [ "log" "level" "build" ]
      [ "log" "level" "proto" ]
    ];
  };

  logLevelType = lib.types.enum [ "trace" "debug" "info" "warn" "error" ];
  environmentValue = value: if lib.isBool value then lib.boolToString value else lib.mapNullable toString value;
in {
  options = lib.recursiveUpdate workerOptions.options {
    enable = lib.mkEnableOption "the Gradient worker";

    environmentVariables = lib.mkOption {
      type = with lib.types; lazyAttrsOf (nullOr (oneOf [ bool int str path ]));
      default = { };
      example = { GRADIENT_WORKER_BUILD_MAX_CONCURRENT = 8; };
      description = ''
        Environment variables of the Gradient worker.
        The [configuration reference](https://wavelens.github.io/gradient/reference/configuration) lists every variable and its default.
        Set a variable to `null` to unset it.
      '';
    };

    packages.gradient = lib.mkPackageOption pkgs "gradient" { };

    peersFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        File of peer tokens for challenge-response authentication with the server,
        with `peer_id:token` per line and `#` starting a comment.

        ```
        <uuid>:<token>
        *:<token>
        ```

        Use peer ID `*` to match any UUID in the server's challenge.
        Tokens are 48 byte random secrets, for example from {command}`openssl rand -base64 48`.
        Register them via the API through `POST /api/v1/projects/{project}/workers`.
        Set it to `null` for open mode without authentication; avoid that in a production setup.
      '';
    };

    acceptedServerTokensFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        File of token hashes required from connecting servers,
        with `peer_id:hash` per line and `#` starting a comment.
        A hash is an argon2 PHC string or the lowercase SHA-256 hex digest of the token itself,
        for example from {command}`printf %s "$TOKEN" | sha256sum`.
        Peer ID `*` matches any peer.
        The worker reads this file only with {env}`GRADIENT_WORKER_DISCOVERABLE`.
        Use `null` to accept every server. This value needs {option}`services.gradient.worker.allowDiscoverableWithoutAcceptedServerTokensFile`.
      '';
    };

    allowDiscoverableWithoutAcceptedServerTokensFile = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Set {option}`services.gradient.worker.discoverable` and leave {option}`services.gradient.worker.acceptedServerTokensFile` unset to accept every server that can connect. This setting is for closed networks only.";
    };

    system = {
      architectures = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ pkgs.stdenv.hostPlatform.system ] ++ lib.optional (pkgs.stdenv.hostPlatform.system == "x86_64-linux") "i686-linux";
        defaultText = lib.literalExpression ''[ pkgs.stdenv.hostPlatform.system ] ++ lib.optional (pkgs.stdenv.hostPlatform.system == "x86_64-linux") "i686-linux"'';
        example = [ "x86_64-linux" "aarch64-linux" ];
        description = "Nix system types this worker builds for.";
      };

      features = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        example = [ "nixos-test" "benchmark" "big-parallel" ];
        description = ''
          Nix system features this worker advertises. An empty list uses the system features of
          the Nix store, including those that enabled experimental features imply, such as
          `recursive-nix`.
        '';
      };
    };

    nixDaemon.maxConnections = lib.mkOption {
      type = lib.types.ints.positive;
      default = cfg.build.maxConcurrent * 9 + 16;
      defaultText = lib.literalExpression "config.services.gradient.worker.build.maxConcurrent * 9 + 16";
      description = ''
        Maximum connections to the local Nix daemon. Each build holds one for its whole run
        plus up to 8 for parallel NAR imports. The rest is headroom for path checks.
      '';
    };

    nar.chunkBytes = lib.mkOption {
      type = lib.types.ints.between 4096 4194304;
      default = 524288;
      description = ''
        Size in bytes of each outbound NAR upload chunk, from 4 KiB to 4 MiB.
        Smaller chunks can help slow or high-latency links like Tailscale over DERP.
      '';
    };

    log.level = lib.mkOption {
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
              Log level of the evaluator. `null` uses
              {option}`services.gradient.worker.log.level.default`.
            '';
          };

          build = lib.mkOption {
            type = lib.types.nullOr logLevelType;
            default = null;
            description = ''
              Log level of the builder. `null` uses
              {option}`services.gradient.worker.log.level.default`.
            '';
          };

          proto = lib.mkOption {
            type = lib.types.nullOr logLevelType;
            default = null;
            description = ''
              Log level of the protocol layer. `null` uses
              {option}`services.gradient.worker.log.level.default`.
            '';
          };
        };
      };
      default = { };
      description = "Log levels per component.";
    };
  };

  assertions = [
    {
      assertion = cfg.capabilities.federate -> cfg.discoverable;
      message = "services.gradient.worker: capabilities.federate and discoverable together, enable both";
    }
    {
      assertion = cfg.discoverable && cfg.acceptedServerTokensFile == null -> cfg.allowDiscoverableWithoutAcceptedServerTokensFile;
      message = "Enable services.gradient.worker.discoverable without acceptedServerTokensFile and the listener can accept all servers that come to the listener. Set acceptedServerTokensFile, or set allowDiscoverableWithoutAcceptedServerTokensFile to allow that behavior.";
    }
    {
      assertion = !(cfg.system.minFreeRamMb != 0 && cfg.eval.maxMemory != null);
      message = "services.gradient.worker.system.minFreeRamMb and services.gradient.worker.eval.maxMemory are alternatives, unset either";
    }
  ];

  environment = {
    NIX_REMOTE = "daemon";
    XDG_CACHE_HOME = "${cfg.baseDir}/www/.cache";
    GRADIENT_WORKER_SSH_BIN = lib.getExe' cfg.packages.ssh "ssh";
  } // workerOptions.environment cfg // lib.mapAttrs (_: environmentValue) cfg.environmentVariables;
}
