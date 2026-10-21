/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, nixpkgs }:
pkgs.runCommand "nixpkgs.tar.gz" { } ''
  tar -C ${nixpkgs} --transform 's,^\.,nixpkgs,S' --sort=name --mtime=@1 \
    --owner=0 --group=0 --numeric-owner -czf $out .
''
