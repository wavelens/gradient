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

      task = lib.mkOption {
        type = lib.types.str;
        description = "Task identifier for the deployments";
        example = "my-project/my-task";
      };

      waitForBuild = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Wait for an in-flight evaluation to produce a deployable build instead
          of giving up when the newest commit is still in CI.

          The service follows the task's live WebSocket
          (`/api/v1/tasks/<task>/live`) and re-checks on every event, so it
          reacts to the build finishing without polling. It stops as soon as the
          deployment is built, the evaluation or the build fails, or the target
          is already running the evaluated system, and it never fails the unit
          for any of those outcomes.

          Waiting is unbounded: a run that outlives its timer simply makes
          systemd skip the next trigger. Disable to exit immediately when
          nothing is built yet.
        '';
      };

      idleRecheckSec = lib.mkOption {
        type = lib.types.int;
        default = 300;
        example = 60;
        description = ''
          Failsafe re-check interval, in seconds, while waiting on the live
          WebSocket. Bounds how long a dropped event can stall a deployment; it
          is not a polling cadence, since events drive the normal path.
        '';
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
        ] ++ lib.optional cfg.waitForBuild pkgs.websocat
        ++ [
          config.nix.package.out
        ];

        serviceConfig = {
          Type = "oneshot";
          Restart = "on-failure";
          User = "root";
          Group = "root";
          LoadCredential = [ "gradient_api_key:${cfg.apiKeyFile}" ];
          TimeoutStartSec = lib.mkIf cfg.waitForBuild "infinity";
        };

        script = ''
          if ! curl --silent --fail --max-time 5 --output /dev/null "${apiUrl}/health"; then
            echo "Error: Cannot reach ${apiUrl}/health"
            exit 1
          fi

          API_KEY=$(cat ${cfg.apiKeyFile})

          api() {
            curl --silent --fail --max-time 10 --header "Authorization: Bearer $API_KEY" "$@"
          }

          # Verdict for the task's newest evaluation: `deploy <path>` once the
          # system is built, `done <reason>` when nothing more can come of it, or
          # `wait` while the evaluation can still produce one.
          resolve() {
            local evaluation entry_points evaluation_id evaluation_status path status current

            evaluation=$(api "${apiUrl}/tasks/${cfg.task}/evaluations?limit=1") || { echo "wait"; return; }
            evaluation_id=$(echo "$evaluation" | jq -r '.message[0].id // empty')
            evaluation_status=$(echo "$evaluation" | jq -r '.message[0].status // empty')

            if [ -z "$evaluation_id" ]; then
              echo "done task ${cfg.task} has no evaluations"
              return
            fi

            entry_points=$(api "${apiUrl}/tasks/${cfg.task}/entry-points?evaluation_id=$evaluation_id") || { echo "wait"; return; }

            # Output paths are written at evaluation time from the resolved .drv,
            # so the deployment is identifiable before, and independently of, its
            # build. Entry points are absent entirely until derivations resolve.
            path=$(echo "$entry_points" | jq -r --arg re '${systemPathRegex}' \
              'first(.message[] | select((.outputs.out // "") | test($re))) | .outputs.out // empty')
            status=$(echo "$entry_points" | jq -r --arg re '${systemPathRegex}' \
              'first(.message[] | select((.outputs.out // "") | test($re))) | .build_status // empty')

            if [ -n "$path" ]; then
              current=$(readlink /run/current-system || true)
              if [ "$path" = "$current" ]; then
                echo "done system is already up-to-date with $path"
                return
              fi

              case "$status" in
                Completed|Substituted)
                  echo "deploy $path"
                  return
                  ;;
                FailedPermanent|FailedTimeout|DependencyFailed|Aborted)
                  echo "done build of $path finished $status"
                  return
                  ;;
              esac
            fi

            case "$evaluation_status" in
              Completed|Failed|Aborted)
                echo "done evaluation $evaluation_id finished $evaluation_status without a deployment for ${cfg.deployFor}"
                ;;
              *)
                echo "wait"
                ;;
            esac
          }

          deploy() {
            echo "New deployment found: $1"
            nix-store --realize "$1"

            nix-env -p /nix/var/nix/profiles/system --set "$1"
            "$1/bin/switch-to-configuration" switch

            echo "Deployment to $1 completed successfully"
          }

          settle() {
            local verdict
            verdict=$(resolve)

            case "$verdict" in
              "deploy "*)
                deploy "''${verdict#deploy }"
                ;;
              "done "*)
                echo "''${verdict#done }"
                ;;
              *)
                return 1
                ;;
            esac
          }

          if settle; then
            exit 0
          fi
        ''
        + lib.optionalString (!cfg.waitForBuild) ''
          echo "Nothing deployable yet for task ${cfg.task}; not waiting"
          exit 0
        ''
        + lib.optionalString cfg.waitForBuild ''
          echo "Task ${cfg.task} is still building; following ${liveUrl}"

          while true; do
            exec 3< <(websocat -U --text --no-close --ping-interval 30 --ping-timeout 90 \
              -H="Authorization: Bearer $API_KEY" "${liveUrl}" </dev/null)
            stream=$!

            # Re-settle on every connect: the socket reports transitions from here
            # on, and a reconnect gap replays nothing.
            if settle; then
              exit 0
            fi

            while read -r -t ${toString cfg.idleRecheckSec} _event <&3 || [ $? -gt 128 ]; do
              if settle; then
                exit 0
              fi
            done

            exec 3<&-
            kill "$stream" 2>/dev/null || true
            wait "$stream" 2>/dev/null || true
            sleep 30
          done
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
