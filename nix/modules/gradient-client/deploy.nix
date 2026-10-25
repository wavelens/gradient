/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, config, ... }: let
  cfg = config.system.gradient-deploy;

  apiUrl = "${cfg.server}/api/v1";

  liveUrl = let
    host = lib.removePrefix "http://" (lib.removePrefix "https://" cfg.server);
    scheme = if lib.hasPrefix "https://" cfg.server then "wss" else "ws";
  in "${scheme}://${host}/api/v1/tasks/${cfg.task}/live";

  systemPathRegex = "^/nix/store/[a-z0-9]{32}-nixos-system-${cfg.deployFor}-[0-9]{2}\\.[0-9]{2}(\\.[0-9]{8}\\.[a-f0-9]+)?$";
in {
  options = {
    system.gradient-deploy = {
      enable = lib.mkEnableOption "pull deployments from Gradient";
      deployFor = lib.mkOption {
        type = lib.types.str;
        default = config.networking.hostName;
        defaultText = lib.literalExpression "config.networking.hostName";
        example = "my-server";
        description = "Name of the deployment configuration to deploy.";
      };

      server = lib.mkOption {
        type = lib.types.str;
        example = "https://gradient.example.com";
        description = "URL of the Gradient server.";
      };

      apiKeyFile = lib.mkOption {
        type = lib.types.str;
        description = "File containing the API key used to authenticate against the server.";
      };

      task = lib.mkOption {
        type = lib.types.str;
        example = "my-project/my-task";
        description = "Task to deploy from, as `<project>/<task>`.";
      };

      waitForBuild = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Whether to wait for an in-flight evaluation to produce a deployable build instead of
          giving up while the newest commit is still in CI.

          Waiting ends once the deployment is built, the evaluation or build has failed, or the
          target already runs the evaluated system. None of these outcomes fails the unit.
          {option}`system.gradient-deploy.websockets` controls how progress is observed. Waiting
          is unbounded, and systemd skips the next trigger while a run is still going.
        '';
      };

      websockets = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Whether to follow the task's WebSocket while waiting instead of polling. Disable it when
          the connection to the server cannot carry a WebSocket upgrade. The service then polls
          every {option}`system.gradient-deploy.pollIntervalSec`. Only used with
          {option}`system.gradient-deploy.waitForBuild`.
        '';
      };

      pollIntervalSec = lib.mkOption {
        type = lib.types.int;
        default = 60;
        example = 300;
        description = ''
          Seconds between checks while waiting with {option}`system.gradient-deploy.websockets`
          disabled.
        '';
      };

      dates = lib.mkOption {
        type = lib.types.str;
        default = "04:00";
        example = "daily";
        description = ''
          How often or when the deployment runs, in the format described in
          {manpage}`systemd.time(7)`.
        '';
      };

      randomizedDelaySec = lib.mkOption {
        type = lib.types.str;
        default = "0";
        example = "45min";
        description = ''
          Maximum random delay before each deployment, as a time span in the format described in
          {manpage}`systemd.time(7)`.
        '';
      };
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [
      (pkgs.writeScriptBin "gradient-update" ''
        systemctl start -v gradient-deploy.service
      '')
    ];

    systemd = {
      services.gradient-deploy = {
        description = "Gradient Deployment Service";
        after = [ "network.target" ];
        wants = [ "network.target" ];
        startAt = cfg.dates;
        restartIfChanged = false;
        unitConfig.X-StopOnRemoval = false;
        environment = {
          inherit (config.environment.sessionVariables) NIX_PATH;
          HOME = "/root";
          GRADIENT_API_KEY = "%d/gradient_api_key";
        }
        // config.nix.envVars
        // config.networking.proxy.envVars
        // {
          GRADIENT_API_URL = apiUrl;
          GRADIENT_API_KEY_FILE = cfg.apiKeyFile;
          GRADIENT_DEPLOY_FOR = cfg.deployFor;
          GRADIENT_LIVE_URL = liveUrl;
          GRADIENT_POLL_INTERVAL_SEC = toString cfg.pollIntervalSec;
          GRADIENT_SYSTEM_PATH_REGEX = systemPathRegex;
          GRADIENT_TASK = cfg.task;
          GRADIENT_WAIT_FOR_BUILD = if cfg.waitForBuild then "1" else "0";
          GRADIENT_WEBSOCKETS = if cfg.websockets then "1" else "0";
        };

        path = with pkgs; [
          coreutils
          curl
          gnutar
          gzip
          jq
          xz.bin
        ] ++ lib.optional (cfg.waitForBuild && cfg.websockets) pkgs.websocat
        ++ [
          config.nix.package.out
        ];

        serviceConfig = {
          Type = "oneshot";
          Restart = "on-failure";
          RestartSec = 10;
          User = "root";
          Group = "root";
          LoadCredential = [ "gradient_api_key:${cfg.apiKeyFile}" ];
          TimeoutStartSec = lib.mkIf cfg.waitForBuild "infinity";
        };

        script = builtins.readFile ./deploy.sh;
      };

      timers.gradient-deploy = {
        description = "Timer for Gradient Deployment Service";
        timerConfig = {
          RandomizedDelaySec = cfg.randomizedDelaySec;
          Persistent = true;
        };
      };
    };
  };
}
