/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */
{
  description = "Test Repository for Gradient Build Server";
  outputs = { self, ... }: {
    # /nix/store/693ll1r48s9y91habhl0li13qxd8bmwf-buildWait5Sec.drv -> /nix/store/pwvabgapxvwwi42grcm0af5j1xaa0hzh-buildWait5Sec
    packages.x86_64-linux.buildWait5Sec = builtins.derivation {
      name = "buildWait5Sec";
      system = "x86_64-linux";
      builder = "/bin/sh";
      args = [ "-c" "echo \"Hello World!\" > $out" ];
    };
  };
}
