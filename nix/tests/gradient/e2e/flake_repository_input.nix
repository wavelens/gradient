/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */
{
  description = "Test Repository for Gradient Build Server";
  inputs.extra = {
    url = "http://server/upstream/input.tar.gz";
    flake = false;
  };
  outputs = { self, extra, ... }: {
    packages.x86_64-linux.default = derivation {
      name = "input-${builtins.readFile "${extra}/value"}";
      system = "x86_64-linux";
      builder = "/bin/sh";
      args = [ "-c" "echo ok > $out" ];
    };
  };
}
