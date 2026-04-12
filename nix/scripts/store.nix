/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, ... }: let
  testPkgs = pkgs.hello;
in with pkgs; runCommand "store-${testPkgs.pname}" { } ''
  mkdir -p $out/nix-support
  echo "file folder $out/store" >> $out/nix-support/hydra-build-products

  ${lib.getExe nix} copy --extra-experimental-features nix-command --offline --to ./test --derivation ${testPkgs.drvPath}

  chmod -R 744 ./test

  mv ./test/nix/store $out/store

  echo "${testPkgs.drvPath}" > $out/output
''
