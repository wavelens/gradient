/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }: let
  stateDir = "/var/lib/gradient-standalone";
  url = "http://localhost:8080";
in {
  imports = [ ../modules/gradient.nix ];

  networking.hostName = "gradient";
  system.stateVersion = "26.05";

  services.gradient = {
    enable = true;
    domain = "localhost";
    serveUrl = url;
    useTls = false;
    postgres.enable = true;
    registration.enable = false;
    secrets = {
      jwtFile = "${stateDir}/jwt";
      cryptFile = "${stateDir}/crypt";
    };

    worker = {
      enable = true;
      capabilities = { fetch = true; eval = true; build = true; };
    };

    state.users.admin = {
      email = "admin@localhost";
      password_file = "${stateDir}/admin-password-hash";
      superuser = true;
    };
  };

  services.postgresql.package = pkgs.postgresql_18;
  nix.settings.sandbox = true;

  systemd.services.gradient-standalone-secrets = {
    wantedBy = [ "multi-user.target" ];
    requiredBy = [ "gradient-server.service" ];
    before = [ "gradient-server.service" ];
    path = [ pkgs.openssl pkgs.libargon2 ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      StateDirectory = "gradient-standalone";
      StateDirectoryMode = "0700";
      UMask = "0077";
    };
    script = ''
      cd ${stateDir}
      [ -e jwt ] || openssl rand -base64 48 > jwt
      [ -e crypt ] || openssl rand -base64 48 > crypt
      if [ ! -e admin-password-hash ]; then
        openssl rand -base64 18 > admin-password
        tr -d '\n' < admin-password | argon2 "$(openssl rand -hex 16)" -id -e > admin-password-hash
      fi
    '';
  };

  systemd.services.gradient-standalone-login = {
    wantedBy = [ "multi-user.target" ];
    after = [ "gradient-server.service" "nginx.service" ];
    requires = [ "gradient-standalone-secrets.service" ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      StandardOutput = "journal+console";
    };
    script = ''
      echo "Gradient is running at ${url} - log in with admin / $(cat ${stateDir}/admin-password)"
    '';
  };
}
