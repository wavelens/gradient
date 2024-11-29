{ lib, pkgs, config, ... }: let
  cfg = config.services.gradient;
in {
  options = {
    services.gradient = {
      enable = lib.mkEnableOption "Enable Gradient";
      package = lib.mkOption {
        description = "The package to use.";
        type = lib.types.package;
        default = pkgs.gradient;
      };

      user = lib.mkOption {
        description = "The group under which Gradient runs.";
        type = lib.types.str;
        default = "gradient";
      };

      group = lib.mkOption {
        description = "The user under which Gradient runs.";
        type = lib.types.str;
        default = "gradient";
      };

      ip = lib.mkOption {
        description = "The IP address on which Gradient listens.";
        type = lib.types.str;
        default = "127.0.0.1";
      };

      port = lib.mkOption {
        description = "The port on which Gradient listens.";
        type = lib.types.int;
        default = 3000;
      };

      jwtSecret = lib.mkOption {
        description = "The secret key used to sign JWTs.";
        type = lib.types.str;
      };

      databaseUrl = lib.mkOption {
        description = "The URL of the database to use.";
        type = lib.types.str;
        default = "postgres://postgres:postgres@localhost:5432/gradient";
      };

      oauthEnable = lib.mkEnableOption "Enable OAuth";
    };
  };

  config = {
    systemd.services.gradient = {
      wantedBy = [ "multi-user.target" ];
      after = [
        "network.target"
        "postgresql.service"
      ];

      serviceConfig = {
        ExecStart = lib.getExe cfg.package;
        StateDirectory = "gradient";
        DynamicUser = true;
        User = cfg.user;
        Group = cfg.group;
        ProtectHome = true;
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        ProtectProc = "invisible";
        ProtectSystem = "strict";
        Restart = "on-failure";
        RestartSec = 10;
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
      };

      environment = {
        GRADIENT_IP = cfg.ip;
        GRADIENT_PORT = toString cfg.port;
        GRADIENT_DATABASE_URL = cfg.databaseUrl;
        GRADIENT_JWT_SECRET = cfg.jwtSecret;
        GRADIENT_MAX_CONCURRENT_EVALUATIONS = toString 1;
        GRADIENT_MAX_CONCURRENT_BUILDS = toString 10;
        GRADIENT_OAUTH_ENABLE = lib.mkForce (if cfg.oauthEnable then "true" else "false");
      };
    };
  };
}
