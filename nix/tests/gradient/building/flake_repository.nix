/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0
 */
{
  description = "Test Repository for Gradient Build Server";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };
  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      ...
    }@inputs:
    flake-utils.lib.eachDefaultSystem (system: {
      packages = { pkgs, ... }: {
        buildWait5Sec = pkgs.stdenv.mkDerivation {
          name = "buildWait5Sec";
          src = ./.;
          buildInputs = [ pkgs.bash ];
          installPhase = ''
            mkdir -p $out/bin
            sleep 5
            echo "echo 'Hello, World!'" > $out/bin/hello.sh
          '';
        };
      };
    });
}
