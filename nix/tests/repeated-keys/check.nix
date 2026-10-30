/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, lib }:
let
  python = pkgs.python3.withPackages (p: [ p.tree-sitter p.tree-sitter-grammars.tree-sitter-nix ]);
  sources = lib.fileset.toSource {
    root = ../../..;
    fileset = lib.fileset.fileFilter (file: file.hasExt "nix") ../../..;
  };
in pkgs.runCommand "gradient-repeated-keys-check" { } ''
  ${python}/bin/python3 -W ignore ${./check.py} ${sources}
  touch $out
''
