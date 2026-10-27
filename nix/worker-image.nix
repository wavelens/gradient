/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ self, nixpkgs }:
{ system ? "x86_64-linux", modules, preScript ? "", postScript ? "" }:
(nixpkgs.lib.nixosSystem {
  modules = [
    ./modules/guest.nix
    {
      gradient.guest = {
        enable = true;
        inherit preScript postScript;
      };
      nixpkgs.hostPlatform = system;
      nixpkgs.overlays = [ self.overlays.default ];
    }
  ] ++ modules;
}).config.system.build.gradientGuest
