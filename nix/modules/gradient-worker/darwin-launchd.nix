/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, config, ... }: let
  cfg = config.services.gradient.worker;
  user = config.users.users._gradient-worker;
  group = config.users.groups._gradient-worker;
  sharedWorkerOptionsModule = import ./options-assertions-environment.nix { inherit lib pkgs cfg; };
  gradientWorkerLogFilePath = "/var/log/gradient-worker.log";
  makeGradientWorkerOwnedDirectoryCommand = mode: directory: "install -d -m ${mode} -o ${toString user.uid} -g ${toString group.gid} ${lib.escapeShellArg directory}";
in {
  imports = [
    { options.services.gradient.worker = sharedWorkerOptionsModule.options; }
  ];

  options.services.gradient.worker.packages = {
    git = lib.mkPackageOption pkgs "git" { } // {
      description = "Git package available to the worker for cloning repositories.";
    };

    ssh = lib.mkPackageOption pkgs "openssh" { } // {
      description = ''
        OpenSSH package used as {env}`GIT_SSH_COMMAND` to fetch private flake inputs.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = sharedWorkerOptionsModule.assertions ++ [
      {
        assertion = cfg.serverUrl != null || cfg.discoverable;
        message = "services.gradient.worker: serverUrl unset without discoverable, set either to connect to a server";
      }
      {
        assertion = !cfg.capabilities.exec;
        message = "services.gradient.worker.capabilities.exec: exec jobs run in a cloud-hypervisor VM, so they require a Linux host. Disable capabilities.exec on macOS.";
      }
    ];

    system.activationScripts.preActivation.text = ''
      ${makeGradientWorkerOwnedDirectoryCommand "0755" cfg.baseDir}
      ${lib.optionalString (cfg.gcrootsDir != "") (makeGradientWorkerOwnedDirectoryCommand "0755" cfg.gcrootsDir)}
      ${lib.optionalString (cfg.log.traceDir != null) (makeGradientWorkerOwnedDirectoryCommand "0750" cfg.log.traceDir)}
      touch ${gradientWorkerLogFilePath}
      chown ${toString user.uid}:${toString group.gid} ${gradientWorkerLogFilePath}
    '';

    launchd.daemons.gradient-worker = {
      path = [ cfg.packages.git cfg.packages.ssh ];
      environment = lib.filterAttrs (_: value: value != null) (lib.optionalAttrs (cfg.peersFile != null) {
        GRADIENT_WORKER_PEERS_FILE = toString cfg.peersFile;
      } // lib.optionalAttrs (cfg.acceptedServerTokensFile != null) {
        GRADIENT_WORKER_ACCEPTED_SERVER_TOKENS_FILE = toString cfg.acceptedServerTokensFile;
      } // sharedWorkerOptionsModule.environment);

      serviceConfig = {
        ProgramArguments = [ (lib.getExe' cfg.packages.gradient "gradient-worker") ];
        UserName = user.name;
        GroupName = group.name;
        WorkingDirectory = cfg.baseDir;
        RunAtLoad = true;
        KeepAlive.SuccessfulExit = false;
        ThrottleInterval = 10;
        SoftResourceLimits.NumberOfFiles = 65535;
        HardResourceLimits.NumberOfFiles = 65535;
        StandardOutPath = gradientWorkerLogFilePath;
        StandardErrorPath = gradientWorkerLogFilePath;
      };
    };

    nix.settings = lib.mkIf config.nix.enable {
      trusted-users = [ user.name ];
      experimental-features = [ "nix-command" "flakes" "ca-derivations" ];
    };

    users = {
      knownGroups = [ group.name ];
      knownUsers = [ user.name ];

      groups._gradient-worker = {
        gid = lib.mkDefault 536;
        description = "Group for Gradient Worker";
      };

      users._gradient-worker = {
        uid = lib.mkDefault 536;
        gid = lib.mkDefault group.gid;
        home = lib.mkDefault cfg.baseDir;
        shell = "/usr/bin/false";
        description = "Gradient Worker user";
      };
    };
  };
}
