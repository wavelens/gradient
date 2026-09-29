/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ ... }: {
  imports = [ ./default.nix ];

  boot.isContainer = true;
  networking = {
    firewall.enable = false;
    useDHCP = false;
  };
}
