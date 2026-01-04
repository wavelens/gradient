/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, lib, ... }: {
  name = "development-backend";
  testScript = { nodes, ... }: ''
    start_all()

    server.succeed("${lib.getExe pkgs.curl} http://gradient.local/api/v1/health -i --fail")

    server.succeed("""
        ${lib.getExe pkgs.curl} \
        -X POST \
        -H "Content-Type: application/json" \
        -d '{"username": "test", "name": "Test User", "email": "test@localhost.localdomain", "password": "password"}' \
        http://gradient.local/api/v1/auth/basic/register
    """)

    token = server.succeed("""
      ${lib.getExe pkgs.curl} \
        -X POST \
        -H "Content-Type: application/json" \
        -d '{"loginname": "test", "password": "password"}' \
        http://gradient.local/api/v1/auth/basic/login \
        | ${lib.getExe pkgs.jq} -rj '.message'
    """)

    print(f"Got Token: {token}")

    server.succeed("${lib.getExe pkgs.gradient-cli} config Server http://gradient.local")
    server.succeed("${lib.getExe pkgs.gradient-cli} config AuthToken ACCESS_TOKEN".replace("ACCESS_TOKEN", token))
    server.succeed("${lib.getExe pkgs.gradient-cli} organization create --name testorg --display-name MyOrganization --description 'My Test Organization'")
  '';

  nodes.server = { config, pkgs, lib, ... }: {
    imports = [
      ../../modules/gradient.nix
    ];

    networking.hosts = {
      "127.0.0.1" = [ "gradient.local" ];
    };

    networking.firewall.enable = false;
    documentation.enable = false;
    nix.settings.substituters = lib.mkForce [ ];
    virtualisation.forwardPorts = [
      {
        from = "host";
        host.port = 2222;
        guest.port = 22;
      }
      {
        from = "host";
        host.port = config.services.postgresql.settings.port;
        guest.port = config.services.postgresql.settings.port;
      }
    ];

    security.pam.services.sshd.allowNullPassword = true;
    services = {
      postgresql = {
        package = pkgs.postgresql_18;
        enableTCPIP = true;
        authentication = ''
          #...
          #type database DBuser origin-address auth-method
          # ipv4
          host  all      all     0.0.0.0/0      trust
          # ipv6
          host all       all     ::0/0        trust
        '';

        settings = {
          log_connections = true;
          logging_collector = true;
          log_disconnections = true;
          log_destination = lib.mkForce "syslog";
        };
      };

      openssh = {
        enable = true;
        settings = {
          PermitRootLogin = "yes";
          PermitEmptyPasswords = "yes";
        };
      };
    };
  };
}
