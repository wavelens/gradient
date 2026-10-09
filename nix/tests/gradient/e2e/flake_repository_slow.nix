/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */
{
  description = "Test Repository for Gradient Build Server";
  inputs.nixpkgs.url = "path:[nixpkgs]";
  outputs = { self, nixpkgs, ... }: let
    pkgs = import nixpkgs { system = "x86_64-linux"; };
    leaf = pkgs.runCommand "slow-leaf" { } "sleep 600; touch $out";
  in {
    packages.x86_64-linux = {
      default = pkgs.runCommand "slow-wrap" { } "cp ${leaf} $out";
      keep = pkgs.runCommand "slow-keep" { } "sleep 600; touch $out";
    };
  };
}
