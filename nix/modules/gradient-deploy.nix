/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, config, ... }: let
  cfg = config.system.gradient-deploy;
in {
  options = {
    system.gradient-deploy = {
      enable = lib.mkEnableOption "Gradient deployment service";
      deployFor = lib.mkOption {
        type = lib.types.str;
        description = "Name of the deployment configuration to use";
        default = config.networking.hostName;
        defaultText = lib.literalExpression "config.networking.hostName";
        example = "my-server";
      };

      server = lib.mkOption {
        type = lib.types.str;
        description = "Address to listen on for incoming deployment requests";
        example = "https://gradient.example.com";
      };

      apiKeyFile = lib.mkOption {
        type = lib.types.str;
        description = "Path to file containing the API key for authenticating deployment requests";
      };

      project = lib.mkOption {
        type = lib.types.str;
        description = "Project identifier for the deployments";
        example = "my-org/my-project";
      };

      # TODO:
      # signedCommit = lib.mkOption {
      #   type = lib.types.bool;
      #   description = "Whether to require signed commits for deployments";
      #   default = false;
      # };

      dates = lib.mkOption {
        type = lib.types.str;
        default = "04:00";
        example = "daily";
        description = ''
          How often or when upgrade occurs. For most desktop and server systems
          a sufficient upgrade frequency is once a day.

          The format is described in
          {manpage}`systemd.time(7)`.
        '';
      };

      randomizedDelaySec = lib.mkOption {
        default = "0";
        type = lib.types.str;
        example = "45min";
        description = ''
          Add a randomized delay before each automatic upgrade.
          The delay will be chosen between zero and this value.
          This value must be a time span in the format specified by
          {manpage}`systemd.time(7)`
        '';
      };
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = let
      triggerUpdate = pkgs.writeScriptBin "gradient-update" ''
        systemctl start gradient-deploy.service
      '';
    in [ triggerUpdate ];

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
        // config.networking.proxy.envVars;

        path = with pkgs; [
          coreutils
          curl
          gnutar
          gzip
          jq
          xz.bin
        ] ++ [
          config.nix.package.out
        ];

        serviceConfig = {
          Type = "oneshot";
          Restart = "on-failure";
          User = "root";
          Group = "root";
          LoadCredential = [ "gradient_api_key:${cfg.apiKeyFile}" ];
        };

        script = ''
          if ! curl --silent --fail --max-time 5 "${cfg.server}/api/v1/health"; then
            echo "Error: Cannot reach ${cfg.server}/api/v1/health"
            exit 1
          fi

          API_KEY=$(cat ${cfg.apiKeyFile})

          PROJECT_DETAILS=$(curl --silent --fail --max-time 10 \
            --header "Authorization: Bearer $API_KEY" \
            "${cfg.server}/api/v1/projects/${cfg.project}/details"
            )

          if [ -z "$PROJECT_DETAILS" ]; then
            echo "Error: No project details received"
            exit 1
          fi

          PROJECT_LAST_EVAL=$(echo "$PROJECT_DETAILS" | jq -r '.message.latest_evaluation.id // empty')
          if [ -z "$PROJECT_LAST_EVAL" ]; then
            echo "Error: No latest_evaluation found for project ${cfg.project}"
            exit 1
          fi

          # Entry-point build IDs are the candidate deployments for this project.
          BUILD_IDS=$(echo "$PROJECT_DETAILS" | jq -r '.message.entry_points[].build_id')
          if [ -z "$BUILD_IDS" ]; then
            echo "Error: No entry points found for evaluation $PROJECT_LAST_EVAL"
            exit 1
          fi

          DEPLOYMENT_STORE_PATH=""

          for BUILD_ID in $BUILD_IDS; do
            BUILD_INFO=$(curl --silent --fail --max-time 10 \
              --header "Authorization: Bearer $API_KEY" \
              "${cfg.server}/api/v1/builds/$BUILD_ID"
              )

            if [ -n "$BUILD_INFO" ]; then
              BUILD_STATUS=$(echo "$BUILD_INFO" | jq -r '.message.status')
              if [ "$BUILD_STATUS" != "Succeeded" ]; then
                continue
              fi
              STORE_PATH=$(echo "$BUILD_INFO" | jq -r '.message.output.out // empty')
              if echo "$STORE_PATH" | grep -qE "^/nix/store/[a-z0-9]{32}-nixos-system-${cfg.deployFor}-[0-9]{2}\.[0-9]{2}(\.[0-9]{8}\.[a-f0-9]+)?$"; then
                DEPLOYMENT_STORE_PATH="$STORE_PATH"
                break
              fi
            fi
          done

          if [ -z "$DEPLOYMENT_STORE_PATH" ]; then
            echo "No deployment found for project ${cfg.project} and server ${cfg.deployFor}"
            exit 0
          fi

          CURRENT_SYSTEM=$(readlink /run/current-system || echo "")
          if [ "$CURRENT_SYSTEM" = "$DEPLOYMENT_STORE_PATH" ]; then
            echo "System is already up-to-date with $DEPLOYMENT_STORE_PATH"
            exit 0
          fi

          echo "New deployment found: $DEPLOYMENT_STORE_PATH"
          nix-store --realize "$DEPLOYMENT_STORE_PATH"

          nix-env -p /nix/var/nix/profiles/system --set "$DEPLOYMENT_STORE_PATH"
          $DEPLOYMENT_STORE_PATH/bin/switch-to-configuration switch

          echo "Deployment to $DEPLOYMENT_STORE_PATH completed successfully"
          exit 0
        '';
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

