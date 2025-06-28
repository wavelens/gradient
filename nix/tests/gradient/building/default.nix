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
      server.wait_for_unit("git-daemon.service")
      print(server.succeed("journalctl -u nix-daemon -n 200 --no-pager"))
      builder.succeed("systemctl restart nix-daemon.service")
      print(builder.succeed("journalctl -u nix-daemon -n 200 --no-pager"))

      print(server.succeed("cat /etc/nix/nix.conf"))
      print(builder.succeed("cat /etc/nix/nix.conf"))

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
      print(server.succeed("${lib.getExe pkgs.git} ls-remote git://server/test"))

      # print(server.succeed("${lib.getExe pkgs.nix} eval --json ./test#buildWait5Sec --store unix:///nix/var/nix/daemon-socket/socket"))
      # print(server.succeed("${lib.getExe pkgs.nix} copy --to ssh://builder /nix/store/693ll1r48s9y91habhl0li13qxd8bmwf-buildWait5Sec.drv"))
      # print(server.succeed("${lib.getExe pkgs.nix} build ./test#buildWait5Sec -L"))
      # print(server.succeed("ls -lah"))

      # add ssh key of gradient organization to builder machine
      org_pub_key = server.succeed("${lib.getExe pkgs.gradient-cli} organization ssh show")[12:].strip()

      print(f"Got Organization Public Key: {org_pub_key}")
      builder.succeed(f"echo '{org_pub_key}' > /home/builder/.ssh/authorized_keys")
      builder.succeed("chown builder:users /home/builder/.ssh/authorized_keys")
      builder.succeed("chmod 600 /home/builder/.ssh/authorized_keys")

      server.succeed("${lib.getExe pkgs.gradient-cli} server create --name testserver --display-name MyServer --host builder --port 22 --ssh-user builder --architectures x86_64-linux --features big-parallel")

      # test connection to build server (to verify ssh key does work as exptected)
      print(server.succeed(f"""
        ${lib.getExe pkgs.curl} -i --fail \
          -X POST \
          -H "Authorization: Bearer {token}" \
          http://gradient.local/api/v1/servers/testorg/testserver/check-connection
      """))

      # create project from git repository
      server.succeed("${lib.getExe pkgs.gradient-cli} project create --name testproject --display-name MyProject --description 'Just a test' --repository git://server/test --evaluation-wildcard packages.*.*")

      # test git repository pullable
      print(server.succeed(f"""
        ${lib.getExe pkgs.curl} -i --fail \
          -X POST \
          -H "Authorization: Bearer {token}" \
          http://gradient.local/api/v1/projects/testorg/testproject/check-repository
      """))

      print(server.succeed("${lib.getExe pkgs.gradient-cli} project show"))

      builder.sleep(30)

      print(server.succeed("${lib.getExe pkgs.gradient-cli} project show"))

      print(server.succeed("cat /nix/store/693ll1r48s9y91habhl0li13qxd8bmwf-buildWait5Sec.drv"))
      builder.succeed("cat /nix/store/693ll1r48s9y91habhl0li13qxd8bmwf-buildWait5Sec.drv || true")

      print(server.succeed("nix path-info /nix/store/693ll1r48s9y91habhl0li13qxd8bmwf-buildWait5Sec.drv"))
      print(server.succeed("NIX_REMOTE=daemon nix path-info /nix/store/693ll1r48s9y91habhl0li13qxd8bmwf-buildWait5Sec.drv"))
      # builder.succeed("cat /nix/store/693ll1r48s9y91habhl0li13qxd8bmwf-buildWait5Sec.drv")

      # project_data = x(f"""
      #   ${lib.getExe pkgs.curl} \
      #     -X GET \
      #     -H "Authorization: Bearer {token}" \
      #     http://gradient.local/api/v1/project/{project_id} \
      #     | ${lib.getExe pkgs.jq} -rj '.message'
      # """)

      # print(f"Got Project Data: {project_data}")

      # print(x("journalctl -u gradient-server -n 100 --no-pager"))
      # print(server.succeed("ssh server ${lib.getExe pkgs.tree} /var/lib/gradient"))

      # TODO wait until project last_evaluation != null
      '';
  });
}
