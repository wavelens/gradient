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