/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, ... }: {
  options = {
    services.gradient.frontend = {
      enable = lib.mkEnableOption "Enable Gradient Frontend";
      package = lib.mkPackageOption pkgs "gradient-frontend" { };
    };
  };
}
