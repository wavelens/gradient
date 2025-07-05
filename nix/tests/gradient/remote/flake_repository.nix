/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */
{
  description = "Test Repository for Gradient Build Server";
  outputs = { self, ... }: {
    packages.x86_64-linux.buildWait5Sec = builtins.derivation {
      name = "buildWait5Sec";
      system = "x86_64-linux";
      builder = "/bin/sh";
      args = [
        "-c"
        ''
          # echo "Hello World!" > "$out/text"
          # echo "file text $out/text" > "$out/nix-support/hydra-build-products"
          echo "Hello World!" > $out
          echo "Done."
        ''
      ];
    };
  };
}
