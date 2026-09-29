/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }: let
  flake = pkgs.replaceVars ./flake_repository.nix { system = pkgs.stdenv.hostPlatform.system; };
in {
  value = pkgs.testers.runNixOSTest {
    name = "gradient-standalone";
    globalTimeout = 1800;

    nodes.machine = { pkgs, lib, ... }: {
      imports = [ ../../../standalone/default.nix ];

      virtualisation = { cores = 4; memorySize = 4096; diskSize = 8192; writableStore = true; };
      nix.settings.substituters = lib.mkForce [ ];
      environment.systemPackages = [ pkgs.curl pkgs.git ];

      services.gitDaemon = { enable = true; basePath = "/srv/git"; exportAll = true; };
      systemd.tmpfiles.rules = [ "d /srv/git 0755 git git" "L+ /srv/flake.nix - - - - ${flake}" ];
    };

    interactive.nodes.machine = import ../../modules/debug-host.nix;

    testScript = builtins.readFile ./checks.py + builtins.readFile ./test.py;
  };
}
