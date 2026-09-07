/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }: {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-deploy";
    globalTimeout = 900;

    nodes = {
      machine = { pkgs, lib, ... }: {
        imports = [ ../../../modules/gradient-deploy.nix ];

        networking.firewall.enable = false;
        documentation.enable = false;
        virtualisation = {
          memorySize = 2048;
          writableStore = true;
        };
        nix.settings.substituters = lib.mkForce [ ];

        # The test framework pins the label to `test`, which is not a version the
        # deploy module's `nixos-system-<host>-<version>` match accepts. Outrank
        # it so both system closures are named the way a real one would be.
        system.nixos.label = lib.mkOverride 10 "25.05.20260907.abcdef1";

        # The deployment target: same host, one extra file, so switching to it
        # is observable without restarting anything the test depends on.
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
          # An hour out, so anything the service notices came from an event.
          idleRecheckSec = 3600;
        };
      };
    };

    interactive.nodes = {
      machine = import ../../modules/debug-host.nix;
    };

    testScript = builtins.readFile ./test.py;
  });
}
