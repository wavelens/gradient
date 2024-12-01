/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0
 */

{ config, ... }:
{
  #imports = [
    # ../../nginx/grafana.nix
  #];

  services.grafana = {
    enable = true;
    settings = {
      analytics.reporting_enabled = false;
      "auth.anonymous" = {
        enabled = true;
        # org_name = "Chaos";
        org_role = "Admin";
      };
      users = {
        allow_sign_up = false;
        login_hint = "admin";
        password_hint = "admin";
      };
      server = {
        http_addr = "127.0.0.1";
        http_port = 3000;
        enforce_domain = false;
        enable_gzip = true;
        domain = "${config.networking.fqdn}";
      };
    };
  };
}
