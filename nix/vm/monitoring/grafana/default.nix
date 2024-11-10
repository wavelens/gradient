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
        org_role = "Viewer";
      };
      users.allow_sign_up = false;
      server = {
        http_addr = "127.0.0.1";
        http_port = 3000;
        enforce_domain = false;
        enable_gzip = true;
        domain = "${config.networking.domain}";
      };
    };
  };
}