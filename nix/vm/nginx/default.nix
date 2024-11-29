{ config, ... }:
{
  services.nginx = {
    enable = true;
    recommendedTlsSettings = true;
    recommendedOptimisation = true;
    recommendedGzipSettings = true;
    commonHttpConfig = ''
      #types_hash_max_size 1024;
      server_names_hash_bucket_size 128;
    '';

    virtualHosts = {
      "${config.networking.domain}" = {
        forceSSL = false;
        globalRedirect = "grafana.${config.networking.fqdn}";
      };
    };
  };
  networking.firewall.allowedTCPPorts = [ 80 ];
}