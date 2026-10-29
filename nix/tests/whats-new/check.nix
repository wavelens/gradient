/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, lib }:
let
  notes = ../../../docs/whats-new;
in pkgs.runCommand "gradient-whats-new-check" { nativeBuildInputs = [ pkgs.nodejs ]; } ''
  mkdir -p frontend/scripts frontend/src/app/core/whats-new docs
  cp ${../../../frontend/scripts/whats-new.mts} frontend/scripts/whats-new.mts
  cp ${../../../frontend/src/app/core/whats-new/release-notes.generated.ts} frontend/src/app/core/whats-new/release-notes.generated.ts
  ${lib.optionalString (builtins.pathExists notes) "cp -r ${notes} docs/whats-new"}
  node frontend/scripts/whats-new.mts --check
  touch $out
''
