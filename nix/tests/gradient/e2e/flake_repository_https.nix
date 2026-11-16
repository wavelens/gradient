/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */
{
  description = "Test repository for the gradient build server.";
  inputs.extra = {
    url = "git+https://server/git/input.git";
    flake = false;
  };
  outputs = { self, extra, ... }: {
    packages.x86_64-linux.default = derivation {
      name = "https-input-${builtins.readFile "${extra}/value"}";
      system = "x86_64-linux";
      builder = "/bin/sh";
      args = [ "-c" "echo ok > $out" ];
    };
  };
}
