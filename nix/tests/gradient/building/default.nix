/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }: {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-building";
    globalTimeout = 480;

    defaults = {
      networking.firewall.enable = false;
      virtualisation.writableStore = true;
      documentation.enable = false;
      nix.settings.substituters = lib.mkForce [ ];
    };

    nodes = {
      server = { config, pkgs, lib, ... }: {
        imports = [
          ../../../modules/gradient.nix
        ];

        systemd.services.gradient-server.environment.GRADIENT_DEBUG = lib.mkForce "true";
        networking.hosts = {
          "127.0.0.1" = [ "gradient.local" ];
        };

        services = {
          gradient = {
            enable = true;
            serveCache = true;
            configureNginx = true;
            configurePostgres = true;
            domain = "gradient.local";
            jwtSecretFile = toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4f8905623866edb2b344bd15de52ccece395b3546e2f00644eb2679cf7bdaa156fd75cc5f47c34448cba19d903e68015b1ad3c8e9d04862de0a2c525b6676779012919fa9551c4746f9323ab207aedae86c28ada67c901cae821eef97b69ca4ebe1260de31add34d8265f17d9c547e3bbabe284d9cadcc22063ee625b104592403368090642a41967f8ada5791cb09703d0762a3175d0fe06ec37822e9e41d0a623a6349901749673735fdb94f2c268ac08a24216efb058feced6e785f34185a");
            cryptSecretFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZAo=");
            settings.logLevel = "info";
          };

          postgresql = {
            package = pkgs.postgresql_17;
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

          gitDaemon = {
            enable = true;
            basePath = "/var/lib/git/";
            exportAll = true;
            options = "--enable=receive-pack";
          };

          openssh = {
            enable = true;
            settings.PasswordAuthentication = false;
          };
        };

        nix.settings = {
          max-jobs = 0;
        };

        systemd.tmpfiles.rules = [
          "d /var/lib/git 0755 git git"
          "L+ /var/lib/git/flake.nix 0755 git git - ${./flake_repository.nix}"
        ];

        users.users.root.openssh.authorizedKeys.keys = [
          "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPQhtH1+yyKLtn4FWkGkaLm1YOlqsJ5dYEw+BKKCeB0f microvm"
        ];
      };

      builder = { config, pkgs, lib, ... }: {
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
    };

    testScript = { nodes, ... }:
      ''
      start_all()

      server.wait_for_unit("gradient-server.service")
      server.sleep(5)
      server.wait_for_unit("git-daemon.service")
      print(server.succeed("journalctl -u nix-daemon -n 200 --no-pager"))
      builder.succeed("systemctl restart nix-daemon.service")
      print(builder.succeed("journalctl -u nix-daemon -n 200 --no-pager"))

      print(server.succeed("cat /etc/nix/nix.conf"))
      print(builder.succeed("cat /etc/nix/nix.conf"))

      # Test health endpoint
      server.succeed("${lib.getExe pkgs.curl} http://gradient.local/api/v1/health -i --fail")

      # Test user registration and authentication
      server.succeed("""
          ${lib.getExe pkgs.curl} \
          -X POST \
          -H "Content-Type: application/json" \
          -d '{"username": "testuser", "name": "Test User", "email": "test@localhost.localdomain", "password": "ctcd5B?t59694"}' \
          http://gradient.local/api/v1/auth/basic/register --fail
      """)

      token = server.succeed("""
        ${lib.getExe pkgs.curl} \
          -X POST \
          -H "Content-Type: application/json" \
          -d '{"loginname": "testuser", "password": "ctcd5B?t59694"}' \
          http://gradient.local/api/v1/auth/basic/login \
          | ${lib.getExe pkgs.jq} -rj '.message'
      """)

      print(f"Got Token: {token}")

      # Test CLI configuration commands
      print("=== Testing CLI Configuration ===")
      server.succeed("${lib.getExe pkgs.gradient-cli} config Server http://gradient.local")
      server.succeed("${lib.getExe pkgs.gradient-cli} config AuthToken ACCESS_TOKEN".replace("ACCESS_TOKEN", token))

      # Test status command
      print(server.succeed("${lib.getExe pkgs.gradient-cli} status"))

      # Test info command
      print(server.succeed("${lib.getExe pkgs.gradient-cli} info"))

      # Test organization commands
      print("=== Testing Organization Commands ===")
      server.succeed("${lib.getExe pkgs.gradient-cli} organization create --name testorg --display-name MyOrganization --description 'My Test Organization'")
      print(server.succeed("${lib.getExe pkgs.gradient-cli} organization show"))
      print(server.succeed("${lib.getExe pkgs.gradient-cli} organization list"))

      # Test organization SSH commands
      print("=== Testing Organization SSH ===")
      org_pub_key = server.succeed("${lib.getExe pkgs.gradient-cli} organization ssh show")[12:].strip()
      print(f"Got Organization Public Key: {org_pub_key}")

      # Test cache commands
      print("=== Testing Cache Commands ===")
      server.succeed("${lib.getExe pkgs.gradient-cli} cache create --name testcache --display-name 'Test Cache' --description 'Test cache description' --priority 10")
      print(server.succeed("${lib.getExe pkgs.gradient-cli} cache list"))
      print(server.succeed("${lib.getExe pkgs.gradient-cli} cache show testcache"))

      # Test organization cache subscription
      server.succeed("${lib.getExe pkgs.gradient-cli} organization cache add testcache")
      print(server.succeed("${lib.getExe pkgs.gradient-cli} organization cache list"))

      # Configure git
      server.succeed("${lib.getExe pkgs.git} config --global --add safe.directory '*'")
      server.succeed("${lib.getExe pkgs.git} config --global init.defaultBranch main")
      server.succeed("${lib.getExe pkgs.git} config --global user.email 'nixos@localhost'")
      server.succeed("${lib.getExe pkgs.git} config --global user.name 'NixOS test'")

      # Initialize git repository
      server.succeed("${lib.getExe pkgs.git} init /var/lib/git/test")
      server.succeed("cp /var/lib/git/{,test/}flake.nix")
      server.succeed("chown git:git -R /var/lib/git/test")
      server.succeed("${lib.getExe pkgs.git} -C /var/lib/git/test add flake.nix")
      server.succeed("${lib.getExe pkgs.git} -C /var/lib/git/test commit -m 'Initial commit'")

      # Ensure git repository is available without authentication
      server.succeed("${lib.getExe pkgs.git} clone git://localhost/test test")
      print(server.succeed("${lib.getExe pkgs.git} ls-remote git://server/test"))

      # Add SSH key to builder machine
      builder.succeed(f"echo '{org_pub_key}' > /home/builder/.ssh/authorized_keys")
      builder.succeed("chown builder:users /home/builder/.ssh/authorized_keys")
      builder.succeed("chmod 600 /home/builder/.ssh/authorized_keys")

      # Test server commands
      print("=== Testing Server Commands ===")
      server.succeed("${lib.getExe pkgs.gradient-cli} server create --name testserver --display-name MyServer --host builder --port 22 --ssh-user builder --architectures x86_64-linux --features big-parallel")
      print(server.succeed("${lib.getExe pkgs.gradient-cli} server list"))

      # Test server connection
      print(server.succeed(f"""
        ${lib.getExe pkgs.curl} -i --fail \
          -X POST \
          -H "Authorization: Bearer {token}" \
          http://gradient.local/api/v1/servers/testorg/testserver/check-connection
      """))

      # Test project commands
      print("=== Testing Project Commands ===")
      server.succeed("${lib.getExe pkgs.gradient-cli} project create --name testproject --display-name MyProject --description 'Just a test' --repository git://server/test --evaluation-wildcard packages.*.buildWait5Sec,packages.*.deployment")
      print(server.succeed("${lib.getExe pkgs.gradient-cli} project list"))
      print(server.succeed("${lib.getExe pkgs.gradient-cli} project show"))

      # Test git repository connectivity
      print(server.succeed(f"""
        ${lib.getExe pkgs.curl} -i --fail \
          -X POST \
          -H "Authorization: Bearer {token}" \
          http://gradient.local/api/v1/projects/testorg/testproject/check-repository
      """))

      # Test project evaluation
      server.succeed("${lib.getExe pkgs.gradient-cli} project evaluate")

      # Wait for evaluation to complete and test cache functionality
      print("=== Testing Nix Cache Functionality ===")
      builder.sleep(120)

      # Check if builds are cached properly
      print(server.succeed("${lib.getExe pkgs.gradient-cli} project show"))

      # Test organization user management
      print("=== Testing Organization User Management ===")
      print(server.succeed("${lib.getExe pkgs.gradient-cli} organization user list"))

      print("=== All Tests Completed Successfully ===")
      '';
  });
}
