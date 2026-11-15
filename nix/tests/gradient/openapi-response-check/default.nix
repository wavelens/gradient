/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }: let
  schemathesis = pkgs.callPackage ../../../packages/schemathesis.nix { };
in {
  value = pkgs.testers.runNixOSTest {
    name = "gradient-openapi-response-check";
    globalTimeout = 1800;

    nodes.machine = {
      imports = [ ../api/shared-test-machine.nix ];

      virtualisation.memorySize = 2048;
      environment.systemPackages = [ schemathesis ];
    };

    interactive.nodes.machine = import ../../modules/debug-host.nix;

    testScript = ''
      start_all()
      machine.wait_for_unit("gradient-server.service")
      machine.wait_for_open_port(3000)

      session_token = machine.wait_until_succeeds(
          "curl -sf -X POST http://localhost:3000/api/v1/auth/basic/login "
          "-H 'Content-Type: application/json' "
          "-d '{\"loginname\": \"stateadmin\", \"password\": \"admin_password\"}' "
          "| jq -er .message",
          timeout=60,
      ).strip()

      machine.succeed(
          f"SCHEMATHESIS_TEST_BEARER_TOKEN={session_token} "
          "st --config-file ${./schemathesis.toml} run ${../../../../docs/gradient-api.yaml} "
          "--url http://localhost:3000 >&2"
      )
    '';
  };
}
