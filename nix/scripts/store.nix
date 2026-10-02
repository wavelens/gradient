/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, skipDirectories ? true, ... }: let
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
      mkdir $out

      jq -r '.closure[] | .path' < "$NIX_ATTRS_JSON_FILE" > $out/store-paths
    '';
  };
in with pkgs; runCommand "store-${testPkgs.pname}" { __structuredAttrs = true; } ''
  mkdir -p $out/nix-support
  echo "file folder $out/store" >> $out/nix-support/hydra-build-products

  mkdir -p $out/store

  ${if skipDirectories then ''
    # The Rust fixture loader is getting only flat files (`.drv` files and source blobs).
    # Directory outputs of derivations are dropped on purpose.
    while read -r path; do
      if [ -f "$path" ]; then
        echo "$path"
      fi
    done < "${closureInfo}/store-paths" | xargs -P 8 -I {} cp {} $out/store
  '' else ''
    # The integration test is getting every path, including directory outputs.
    # The worker VM must find the full build closure already in its local store.
    xargs -a "${closureInfo}/store-paths" -P 8 -I {} cp -a --reflink=auto {} $out/store/
  ''}

  echo "${testPkgs.drvPath}" > $out/output
''
