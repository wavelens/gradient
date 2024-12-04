/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0
 */

{ pkgs, lib, ... }:
{
  name = "gradient-building";
  globalTimeout = 120;

  defaults = {
    networking.firewall.enable = false;
  };

  nodes = {
    server =
      {
        config,
        pkgs,
        lib,
        ...
      }:
      {
        imports = [ ../../../modules/gradient.nix ];
        services = {
          gradient = {
            enable = true;
            ip = "0.0.0.0";
            domain = "gradient.local";
            jwtSecret = "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4f8905623866edb2b344bd15de52ccece395b3546e2f00644eb2679cf7bdaa156fd75cc5f47c34448cba19d903e68015b1ad3c8e9d04862de0a2c525b6676779012919fa9551c4746f9323ab207aedae86c28ada67c901cae821eef97b69ca4ebe1260de31add34d8265f17d9c547e3bbabe284d9cadcc22063ee625b104592403368090642a41967f8ada5791cb09703d0762a3175d0fe06ec37822e9e41d0a623a6349901749673735fdb94f2c268ac08a24216efb058feced6e785f34185a";
            cryptSecret = "aW52YWxpZAo=";
            binpath_nix = lib.getExe pkgs.nix;
            binpath_git = lib.getExe pkgs.git;
          };

          postgresql = {
            enable = true;
            package = pkgs.postgresql_17;
            enableJIT = true;
            enableTCPIP = true;
            ensureDatabases = [ "gradient" ];
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

          gitDaemon = {
            enable = true;
            basePath = "/var/lib/git/";
            exportAll = true;
            options = "--enable=receive-pack";
          };
        };

        systemd.tmpfiles.rules = [
          "d /var/lib/git 0755 git git"
          "L+ /var/lib/git/flake.nix 0755 git git - ${./flake_repository.nix}"
        ];
      };

    builder =
      {
        config,
        pkgs,
        lib,
        ...
      }:
      {
        users.users.builder = {
          isNormalUser = true;
          group = "users";
        };

        nix.settings = {
          experimental-features = [
            "nix-command"
            "flakes"
            "ca-derivations"
          ];
          trusted-users = [
            "root"
            "@wheel"
            "builder"
          ];
        };

        systemd.tmpfiles.rules = [
          "d /home/builder/.ssh 0700 builder users"
        ];

        services.openssh = {
          enable = true;
          settings = {
            PasswordAuthentication = false;
            # rust lib ssh2 requires one of the following Message Authentication Codes:
            # hmac-sha2-256,hmac-sha2-512,hmac-sha1,hmac-sha1-96,hmac-md5,hmac-md5-96,hmac-ripemd160,hmac-ripemd160@openssh.com
            Macs = [
              "hmac-sha2-512"
            ];
          };
        };
      };
  };

  interactive.nodes = {
    server = import ../../modules/debug-host.nix;
    builder = import ../../modules/debug-host.nix;
  };

  testScript =
    { nodes, ... }:
    ''
      start_all()

      for m in [builder, server]:
        m.wait_for_unit("network-online.target")

      server.wait_for_unit("gradient-server.service")
      server.wait_for_unit("git-daemon.service")

      server.succeed("${lib.getExe pkgs.curl} http://localhost:3000/api/health -i --fail")

      builder.succeed("""
        ${lib.getExe pkgs.curl} \
          -X POST \
          -H "Content-Type: application/json" \
          -d '{"username": "test", "name": "Test User", "email": "test@localhost.localdomain", "password": "password"}' \
          http://server:3000/api/user/register
      """)

      token = builder.succeed("""
        ${lib.getExe pkgs.curl} -v \
          -X POST \
          -H "Content-Type: application/json" \
          -d '{"loginname": "test", "password": "password"}' \
          http://server:3000/api/user/login \
          | ${lib.getExe pkgs.jq} -rj '.message'
      """)

      print(f"Got Token: {token}")

      org_id = builder.succeed("""
        ${lib.getExe pkgs.curl} -v \
          -X POST \
          -H "Authorization: Bearer ACCESS_TOKEN" \
          -H "Content-Type: application/json" \
          -d '{"name": "MyOrganization", "description": "My Organization"}' \
          http://server:3000/api/organization \
          | ${lib.getExe pkgs.jq} -rj '.message'
      """.replace("ACCESS_TOKEN", token))

      print(f"Got Org ID: {org_id}")

      # configure git
      server.succeed("${lib.getExe pkgs.git} config --global --add safe.directory '*'")
      server.succeed("${lib.getExe pkgs.git} config --global init.defaultBranch main")
      server.succeed("${lib.getExe pkgs.git} config --global user.email 'nixos@localhost'")
      server.succeed("${lib.getExe pkgs.git} config --global user.name 'NixOS test'")

      # initialize git repository
      server.succeed("${lib.getExe pkgs.git} init /var/lib/git/test")
      server.succeed("cp /var/lib/git/{,test/}flake.nix")
      server.succeed("chown git:git -R /var/lib/git/test")
      server.succeed("${lib.getExe pkgs.git} -C /var/lib/git/test add flake.nix")
      server.succeed("${lib.getExe pkgs.git} -C /var/lib/git/test commit -m 'Initial commit'")

      # ensure git repository is available without authentication
      server.succeed("${lib.getExe pkgs.git} clone git://localhost/test test")
      print(builder.succeed("${lib.getExe pkgs.git} ls-remote git://server/test"))
      builder.succeed("${lib.getExe pkgs.git} clone git://server/test test")

      # add ssh key of gradient organization to builder machine
      builder.succeed(f"""
        ${lib.getExe pkgs.curl} -v \
          -H "Authorization: Bearer {token}" \
          http://server:3000/api/organization/{org_id}/ssh \
          | ${lib.getExe pkgs.jq} -r '.message' \
          > ${nodes.builder.users.users.builder.home}/.ssh/authorized_keys
      """)
      builder.succeed("chown builder:users ${nodes.builder.users.users.builder.home}/.ssh/authorized_keys")
      builder.succeed("chmod 600 ${nodes.builder.users.users.builder.home}/.ssh/authorized_keys")

      server_id = builder.succeed("""
        ${lib.getExe pkgs.curl} -v \
          -X POST \
          -H "Authorization: Bearer ACCESS_TOKEN" \
          -H "Content-Type: application/json" \
          -d '{"name": "MyServer", "host": "builder", "port": 22, "username": "builder", "organization_id": "ORG_ID", "architectures": ["x86_64-linux"], "features": ["big-parallel"]}' \
          http://server:3000/api/server \
          | ${lib.getExe pkgs.jq} -rj '.message'
      """.replace("ACCESS_TOKEN", token).replace("ORG_ID", org_id))

      # test connection to build server (to verify ssh key does work as exptected)
      print(builder.succeed(f"""
        ${lib.getExe pkgs.curl} -v -i \
          -X POST \
          -H "Authorization: Bearer {token}" \
          http://server:3000/api/server/{server_id}/check
      """))

      # create project from git repository
      project_id = builder.succeed("""
        ${lib.getExe pkgs.curl} -v \
          -X POST \
          -H "Authorization: Bearer ACCESS_TOKEN" \
          -H "Content-Type: application/json" \
          -d '{"name": "TestProject", "description": "Just a test", "repository": "git://server/test"}' \
          http://server:3000/api/organization/ORG_ID \
          | ${lib.getExe pkgs.jq} -rj '.message'
      """.replace("ACCESS_TOKEN", token).replace("ORG_ID", org_id))

      # test git repository pullable
      print(builder.succeed(f"""
        ${lib.getExe pkgs.curl} -v -i \
          -X POST \
          -H "Authorization: Bearer {token}" \
          http://server:3000/api/project/{project_id}/check-repository
      """))

      builder.sleep(10)

      project_data = builder.succeed(f"""
        ${lib.getExe pkgs.curl} -v \
          -X GET \
          -H "Authorization: Bearer {token}" \
          http://server:3000/api/project/{project_id} \
          | ${lib.getExe pkgs.jq} -rj '.message'
      """)

      print(f"Got Project Data: {project_data}")

      print(server.succeed("journalctl -u gradient-server -n 100 --no-pager"))

      # TODO wait until project last_evaluation != null
    '';
}
