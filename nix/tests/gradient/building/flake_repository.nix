/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0
 */
{
  description = "Test Repository for Gradient Build Server";
  outputs = { self, ... }: {
    packages.x86_64-linux.buildWait5Sec = builtins.derivation {
      name = "buildWait5Sec";
      system = "x86_64-linux";
      builder = "/bin/sh";
      args = [ "-c" "sleep 5 && echo hello world > $out" ];
    };
  };
}
