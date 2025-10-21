/*
 * SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }: {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-remote";
    globalTimeout = 480;

    defaults = {
      networking.firewall.enable = false;
      documentation.enable = false;
    };

    nodes = {
      server = { config, pkgs, lib, ... }: {
        imports = [
          ../../../modules/gradient.nix
        ];

        systemd.services.gradient-server.environment.GRADIENT_DEBUG = lib.mkForce "true";
        virtualisation.writableStore = true;
        networking.hosts = {
          "127.0.0.1" = [ "gradient.local" ];
        };

        nix.settings = {
          substituters = lib.mkForce [ ];
          trusted-users = [ "builder" ];
        };

        users.users.builder = {
          isNormalUser = true;
          group = "users";
        };

        services = {
          gradient = {
            enable = true;
            serveCache = true;
            configureNginx = true;
            configurePostgres = true;
            domain = "gradient.local";
            jwtSecretFile = toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4f8905623866edb2b344bd15de52ccece395b3546e2f00644eb2679cf7bdaa156fd75cc5f47c34448cba19d903e68015b1ad3c8e9d04862de0a2c525b6676779012919fa9551c4746f9323ab207aedae86c28ada67c901cae821eef97b69ca4ebe1260de31add34d8265f17d9c547e3bbabe284d9cadcc22063ee625b104592403368090642a41967f8ada5791cb09703d0762a3175d0fe06ec37822e9e41d0a623a6349901749673735fdb94f2c268ac08a24216efb058feced6e785f34185a");
            cryptSecretFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK");
            settings = {
              logLevel = "debug";
            };
          };

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
            settings.PasswordAuthentication = false;
          };
        };

        systemd.tmpfiles.rules = [
          "d /home/builder/.ssh 0700 builder users"
        ];

        users.users.root.openssh.authorizedKeys.keys = [
          "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPQhtH1+yyKLtn4FWkGkaLm1YOlqsJ5dYEw+BKKCeB0f microvm"
        ];
      };

      client = { config, pkgs, lib, ... }: {
        nix.enable = false;
        networking.hosts = {
          "192.168.1.2" = [ "gradient.local" ];
        };

        users.users.builder = {
          isNormalUser = true;
          group = "users";
        };

        environment.systemPackages = with pkgs; [
          git
        ];

        systemd.tmpfiles.rules = [
          "d /home/builder/test-repo 0755 builder users"
          "L+ /home/builder/test-repo/flake.nix 0644 builder users - ${./flake_repository.nix}"
        ];
      };
    };

    interactive.nodes = {
      server = import ../../modules/debug-host.nix;
    };

    testScript = { nodes, ... }:
      ''
      start_all()

      server.wait_for_unit("gradient-server.service")
      server.sleep(5)
      print(server.succeed("journalctl -u nix-daemon -n 200 --no-pager"))
      print(server.succeed("cat /etc/nix/nix.conf"))

      # Test health endpoint from client
      client.succeed("${lib.getExe pkgs.curl} http://gradient.local/api/v1/health -i --fail")

      # Test user registration and authentication from client
      client.succeed("""
          ${lib.getExe pkgs.curl} \\
          -X POST \\
          -H "Content-Type: application/json" \\
          -d '{"username": "testuser", "name": "Test User", "email": "test@localhost.localdomain", "password": "ctcd5B?t59694"}' \\
          http://gradient.local/api/v1/auth/basic/register --fail
      """)

      token = client.succeed("""
        ${lib.getExe pkgs.curl} \\
          -X POST \\
          -H "Content-Type: application/json" \\
          -d '{"loginname": "testuser", "password": "ctcd5B?t59694"}' \\
          http://gradient.local/api/v1/auth/basic/login \\
          | ${lib.getExe pkgs.jq} -rj '.message'
      """)

      print(f"Got Token: {token}")

      # Test CLI configuration commands from client
      print("=== Testing CLI Configuration from Client ===")
      client.succeed("${lib.getExe pkgs.gradient-cli} config Server http://gradient.local")
      client.succeed("${lib.getExe pkgs.gradient-cli} config AuthToken ACCESS_TOKEN".replace("ACCESS_TOKEN", token))

      # Test status command from client
      print(client.succeed("${lib.getExe pkgs.gradient-cli} status"))

      # Test info command from client
      print(client.succeed("${lib.getExe pkgs.gradient-cli} info"))

      # Test organization commands from client
      print("=== Testing Organization Commands from Client ===")
      client.succeed("${lib.getExe pkgs.gradient-cli} organization create --name testorg --display-name MyOrganization --description 'My Test Organization'")
      print(client.succeed("${lib.getExe pkgs.gradient-cli} organization show"))
      print(client.succeed("${lib.getExe pkgs.gradient-cli} organization list"))

      # Test organization SSH commands from client
      print("=== Testing Organization SSH from Client ===")
      org_pub_key = client.succeed("${lib.getExe pkgs.gradient-cli} organization ssh show")[12:].strip()
      print(f"Got Organization Public Key: {org_pub_key}")

      print("=== Adding Server ===")
      server.succeed(f"echo '{org_pub_key}' > /home/builder/.ssh/authorized_keys")
      server.succeed("chown builder:users /home/builder/.ssh/authorized_keys")
      server.succeed("chmod 600 /home/builder/.ssh/authorized_keys")

      client.succeed("${lib.getExe pkgs.gradient-cli} server create --name testserver --display-name MyServer --host localhost --port 22 --ssh-user builder --architectures x86_64-linux --features big-parallel")
      print(client.succeed("${lib.getExe pkgs.gradient-cli} server list"))

      # Test server connection
      print(client.succeed(f"""
        ${lib.getExe pkgs.curl} -i --fail \
          -X POST \
          -H "Authorization: Bearer {token}" \
          http://gradient.local/api/v1/servers/testorg/testserver/check-connection
      """))

      # Initialize git repository in client's home directory
      print("=== Setting up Git Repository ===")
      client.succeed("${lib.getExe pkgs.git} config --global --add safe.directory '*'")
      client.succeed("${lib.getExe pkgs.git} config --global init.defaultBranch main")
      client.succeed("${lib.getExe pkgs.git} config --global user.email 'test@localhost'")
      client.succeed("${lib.getExe pkgs.git} config --global user.name 'Test User'")

      client.succeed("cd /home/builder/test-repo && ${lib.getExe pkgs.git} init")
      client.succeed("cd /home/builder/test-repo && ${lib.getExe pkgs.git} add flake.nix")
      client.succeed("cd /home/builder/test-repo && ${lib.getExe pkgs.git} commit -m 'Initial commit with flake.nix'")
      print(client.succeed("cd /home/builder/test-repo && ls -la"))

      print("Git repository initialized in /home/builder/test-repo")

      # Test organization user management from client
      print("=== Testing Organization User Management from Client ===")
      print(client.succeed("cd /home/builder/test-repo && ${lib.getExe pkgs.gradient-cli} build .#packages.x86_64-linux.buildWait5Sec --organization testorg"))

      # print(client.succeed("cd /home/builder/test-repo && ${lib.getExe pkgs.gradient-cli} download -f text"))
      '';
  });
}
