/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }: let
  target = { lib, pkgs, ... }: {
    imports = [ ../../../modules/gradient-client ];

    networking.firewall.enable = false;
    documentation.enable = false;
    virtualisation = {
      memorySize = 2048;
      writableStore = true;
    };
    nix.settings.substituters = lib.mkForce [ ];

    # The test framework is pinning the label to `test`.
    # The deploy module's `nixos-system-<host>-<version>` match is rejecting that version.
    # The override is naming both system closures like a real one.
    system.nixos.label = lib.mkOverride 10 "25.05.20260907.abcdef1";

    specialisation.deployed.configuration = {
      environment.etc."gradient-deployed".text = "deployed";
    };

    environment.systemPackages = with pkgs; [ curl jq ];
    environment.etc."gradient-deploy-api-key".text = "stub-api-key";

    systemd.services.gradient-stub-api = {
      description = "Scripted Gradient API for the deploy test";
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        ExecStart = "${pkgs.python3}/bin/python3 ${./stub_api.py} 8090";
        Restart = "always";
      };
    };

    system.gradient-deploy = {
      enable = true;
      server = "http://127.0.0.1:8090";
      apiKeyFile = "/etc/gradient-deploy-api-key";
      task = "wavelens/dotfiles";
    };

    systemd.services.gradient-deploy.serviceConfig.Restart = lib.mkForce "no";
  };

  microvmRunner = name: pkgs.runCommand "microvm-runner-${name}" { } ''
    mkdir -p "$out/bin"
    printf '#!/bin/sh\nexit 0\n' > "$out/bin/microvm-run"
    chmod +x "$out/bin/microvm-run"
  '';

  microvmRunners = {
    old = microvmRunner "old";
    new = microvmRunner "new";
    newer = microvmRunner "newer";
    invalid = pkgs.runCommand "microvm-runner-invalid" { } "mkdir -p $out/bin";
  };

  microvmTarget = { ... }: {
    imports = [ target ];

    environment.etc."microvm-runners".text = builtins.toJSON
      (builtins.mapAttrs (_: runner: toString runner) microvmRunners);

    system.gradient-deploy = {
      type = "microvm";
      microvm.hostnames = [ "guest" "second-guest" ];
    };

    systemd.tmpfiles.rules = [
      "d /nix/var/nix/gcroots/microvm 0755 root root -"
      "L+ /nix/var/nix/gcroots/microvm/guest - - - - /var/lib/microvms/guest/current"
      "L+ /nix/var/nix/gcroots/microvm/second-guest - - - - /var/lib/microvms/second-guest/current"
      "d /var/lib/microvms/guest 0775 root root -"
      "L+ /var/lib/microvms/guest/current - - - - ${microvmRunners.old}"
      "L+ /var/lib/microvms/guest/booted - - - - ${microvmRunners.old}"
      "d /var/lib/microvms/second-guest 0775 root root -"
      "L+ /var/lib/microvms/second-guest/current - - - - ${microvmRunners.old}"
      "L+ /var/lib/microvms/second-guest/booted - - - - ${microvmRunners.old}"
    ];

    systemd.services."microvm@" = {
      description = "Mock MicroVM service for the deployment test";
      scriptArgs = "%i";
      script = ''
        if [[ -e /run/fail-microvm-restart ]]; then
          exit 1
        fi
        runner=$(readlink -f "/var/lib/microvms/$1/current")
        ln -sfnT "$runner" "/var/lib/microvms/$1/booted"
        echo "$runner" >> /run/microvm-runs
      '';
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
    };
  };
in {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-deploy";
    globalTimeout = 900;

    nodes = {
      machine = target;
      microvm = microvmTarget;

      # The same target with the live socket off is covering the fallback for networks without
      # WebSocket upgrades.
      poller = { ... }: {
        imports = [ target ];
        system.gradient-deploy = {
          websockets = false;
          pollIntervalSec = 3;
        };
      };
    };

    interactive.nodes = {
      machine = import ../../modules/debug-host.nix;
    };

    testScript = builtins.readFile ./test.py;
  });
}
