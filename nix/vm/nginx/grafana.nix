/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ config, ... }:
{
  services.nginx = {
    virtualHosts = {
     "grafana.${config.networking.domain}" = {
       forceSSL = false;
        locations."/"  = {
          proxyPass = "http://${toString config.services.grafana.settings.server.http_addr}:${toString config.services.grafana.settings.server.http_port}";
          proxyWebsockets = true;
          extraConfig = "proxy_pass_header Authorization;";
          recommendedProxySettings = true;
        };
      };
    };
  };
}
