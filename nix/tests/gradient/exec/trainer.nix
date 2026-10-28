/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, ... }: {
  gradient.guest.enable = true;
  system.stateVersion = "26.05";
  # The version suffix follows the nixpkgs revision, which the tarball in the test repository lacks.
  system.nixos.versionSuffix = lib.mkForce "";
  system.nixos.revision = lib.mkForce null;
  environment.etc."gradient-image".text = "trainer";
}
