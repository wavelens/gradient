/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */
{
  description = "Multinode evalbench flake with large package sets";
  inputs.nixpkgs.url = "http://server/nixpkgs.tar.gz";
  outputs = { self, nixpkgs, ... }: let
    pkgs = import nixpkgs { system = "x86_64-linux"; };
  in {
    hydraJobs = {
      perl = pkgs.perlPackages;
      python = pkgs.python3Packages;
    };
  };
}
