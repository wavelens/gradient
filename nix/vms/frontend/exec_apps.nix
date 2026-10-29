/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs }: {
  ticks = pkgs.writeShellScript "dev-ticks" ''
    for i in $(${pkgs.coreutils}/bin/seq 1 600); do
      echo "tick $i"
      ${pkgs.coreutils}/bin/sleep 1
    done
  '';
}
