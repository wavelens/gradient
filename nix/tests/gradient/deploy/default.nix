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
  };
in {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-deploy";
    globalTimeout = 900;

    nodes = {
      machine = target;

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
