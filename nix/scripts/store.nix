/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }: let
  testPkgs = pkgs.hello;
  closureInfo = pkgs.stdenvNoCC.mkDerivation {
    name = "closure-info";

    __structuredAttrs = true;

    exportReferencesGraph.closure = [ testPkgs.drvPath ];

    preferLocalBuild = true;

    nativeBuildInputs = with pkgs; [
      coreutils
      jq
    ];

    buildCommand = ''
      out=''${outputs[out]}
      mkdir $out

      jq -r '.closure[] | select(.ca != null) | .path' < "$NIX_ATTRS_JSON_FILE" > $out/store-paths
    '';
  };
in with pkgs; runCommand "store-${testPkgs.pname}" { } ''
  mkdir -p $out/nix-support
  echo "file folder $out/store" >> $out/nix-support/hydra-build-products

  mkdir -p $out/store

  while read -r path; do
    if [ -f "$path" ]; then
      echo "$path"
    fi
  done < "${closureInfo}/store-paths" | xargs -P 8 -I {} cp {} $out/store

  echo "${testPkgs.drvPath}" > $out/output
''
