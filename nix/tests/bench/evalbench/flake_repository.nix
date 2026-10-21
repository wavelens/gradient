/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */
{
  description = "Benchmark repository with a tarball nixpkgs input";
  inputs.nixpkgs.url = "http://server:8088/nixpkgs.tar.gz";
  outputs = { self, nixpkgs, ... }: let
    pkgs = import nixpkgs { system = "x86_64-linux"; };
  in {
    packages.x86_64-linux = {
      default = pkgs.hello;
    };
  };
}
