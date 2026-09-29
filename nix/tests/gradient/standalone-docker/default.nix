/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ self, pkgs, ... }: let
  image = self.packages.${pkgs.stdenv.hostPlatform.system}.standalone-image;
  flake = pkgs.replaceVars ../standalone/flake_repository.nix { system = pkgs.stdenv.hostPlatform.system; };
in {
  value = pkgs.testers.runNixOSTest {
    name = "gradient-standalone-docker";
    globalTimeout = 2700;

    nodes.machine = { pkgs, ... }: {
      virtualisation = {
        cores = 4;
        memorySize = 6144;
        diskSize = 16384;
        docker.enable = true;
      };
      networking.firewall.trustedInterfaces = [ "docker0" ];
      environment.systemPackages = [ pkgs.curl pkgs.git ];
      environment.etc."gradient-standalone.tar.gz".source = image;

      services.gitDaemon = { enable = true; basePath = "/srv/git"; exportAll = true; };
      systemd.tmpfiles.rules = [ "d /srv/git 0755 git git" "L+ /srv/flake.nix - - - - ${flake}" ];
    };

    testScript = builtins.readFile ../standalone/checks.py + builtins.readFile ./test.py;
  };
}
