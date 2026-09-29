/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs }:
pkgs.runCommand "evalbench-summarize" { nativeBuildInputs = [ pkgs.python3 ]; } ''
  cp -r ${./.} src
  chmod -R u+w src
  cd src
  python3 -m unittest -v test_summarize
  touch $out
''
