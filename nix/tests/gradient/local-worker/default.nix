/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }: let
  # argon2id PHC hash of "admin_password", same fixture the api test uses.
  statePwHash = pkgs.writeText "state-pw-hash" "$argon2id$v=19$m=4096,t=3,p=1$c29tZXNhbHQxMjM0NQ$hIKBEy9SOWlnAlcwUv2PLPBdsMkKhVlCyjTxaWIK+v4";
in {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-local-worker";
    globalTimeout = 900;

    nodes = {
      machine = { pkgs, lib, ... }: {
        imports = [ ../../../modules/gradient.nix ];

        environment.systemPackages = with pkgs; [ curl jq ];

        services.gradient = {
          enable = true;
          frontend.enable = false;
          useTls = false;
          configurePostgres = true;
          domain = "gradient.local";
          jwtSecretFile = toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4");
          cryptSecretFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK");
          settings.logLevel.default = "debug";

          state.users.admin = {
            email = "admin@gradient.local";
            password_file = toString statePwHash;
            email_verified = true;
            superuser = true;
          };
        };

        # The whole point of the test: enabling the worker is the entire
        # configuration. No workerId, no token, no peers file, no UI step.
        services.gradient.worker.enable = true;
      };
    };

    interactive.nodes = {
      machine = import ../../modules/debug-host.nix;
    };

    testScript = builtins.readFile ./test.py;
  });
}
