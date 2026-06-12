/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, lib, ... }: let
  # Same fixture secrets the integration tests use; admin password is "admin_password".
  adminPwHash = pkgs.writeText "admin-pw-hash" "$argon2id$v=19$m=4096,t=3,p=1$c29tZXNhbHQxMjM0NQ$hIKBEy9SOWlnAlcwUv2PLPBdsMkKhVlCyjTxaWIK+v4";
  orgSshKey = pkgs.writeText "org-ssh-key" ''
    -----BEGIN OPENSSH PRIVATE KEY-----
    b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
    QyNTUxOQAAACDle/PUDDuuI9h8+ViFyHMQjqARSRhLJcYKnay7MrflOgAAAJALQNCyC0DQ
    sgAAAAtzc2gtZWQyNTUxOQAAACDle/PUDDuuI9h8+ViFyHMQjqARSRhLJcYKnay7MrflOg
    AAAEAROowXB/e8+691yZgfHOASTPVyIM2Hx7U9RpmAtUda++V789QMO64j2Hz5WIXIcxCO
    oBFJGEslxgqdrLsyt+U6AAAABm5vbmFtZQECAwQFBgc=
    -----END OPENSSH PRIVATE KEY-----
  '';
  workerToken = pkgs.writeText "worker-token" "C9ve6tvVONhtbRzFks56HQlYQotlRmXel/5NFLk/HjbSFGc+IZjCGfxegW2NKpY5";
  cacheSigningKey = pkgs.writeText "cache-signing-key" "22yRW7p/hxuPRWJh9pcfGH0oXPk2MFUuG0wIA1rfq1BvDbvMqzMZS+er/BE8ucbxNSG5KZ8B0ELO4TJal8mZlw==";
in {
  name = "development-frontend";
  testScript = { nodes, ... }: ''
    start_all()
    server.wait_for_unit("gradient-server.service")
    server.wait_for_unit("git-daemon.service")
    server.wait_for_open_port(3000)
    server.wait_for_unit("nginx.service")

    server.succeed("${lib.getExe pkgs.curl} http://gradient.local/api/v1/health -i --fail")

    server.succeed("${lib.getExe pkgs.git} config --global --add safe.directory '*'")
    server.succeed("${lib.getExe pkgs.git} config --global init.defaultBranch main")
    server.succeed("${lib.getExe pkgs.git} config --global user.email 'nixos@localhost'")
    server.succeed("${lib.getExe pkgs.git} config --global user.name 'NixOS test'")

    server.succeed("${lib.getExe pkgs.git} init /var/lib/git/test")
    server.succeed("cp /var/lib/git/{,test/}flake.nix")
    server.succeed("chown git:git -R /var/lib/git/test")
    server.succeed("${lib.getExe pkgs.git} -C /var/lib/git/test add flake.nix")
    server.succeed("${lib.getExe pkgs.git} -C /var/lib/git/test commit -m 'Initial commit'")

    server.wait_for_unit("gradient-worker.service")

    print("Dev environment ready: frontend proxy target http://localhost:3000")
    print("Login: admin / admin_password")
  '';

  interactive = {
    sshBackdoor.enable = true;
    nodes.server.virtualisation.graphics = false;
  };

  nodes.server = { config, pkgs, lib, ... }: {
    imports = [
      ../../modules/gradient.nix
      ../../modules/gradient-worker.nix
    ];

    networking.hosts = {
      "127.0.0.1" = [ "gradient.local" ];
    };

    networking.firewall.enable = false;
    documentation.enable = false;
    virtualisation = {
      cores = 4;
      memorySize = 4096;
      diskSize = 4096;
      writableStore = true;
      forwardPorts = [
        {
          from = "host";
          host.port = 2222;
          guest.port = 22;
        }
        {
          from = "host";
          host.port = 3000;
          guest.port = 80;
        }
      ];
    };

    nix.settings = {
      substituters = lib.mkForce [ ];
      max-jobs = lib.mkForce 4;
    };

    # Pre-seed a deterministic worker UUID so the server state config
    # can register it before the worker boots.
    systemd.tmpfiles.rules = [
      "d /var/lib/git 0755 git git"
      "L+ /var/lib/git/flake.nix 0755 git git - ${./flake_repository.nix}"
      "d /var/lib/gradient-worker 0755 gradient-worker gradient-worker"
      "f /var/lib/gradient-worker/worker-id 0644 gradient-worker gradient-worker - a0000000-0000-0000-0000-000000000001"
    ];

    environment.etc."gradient/secrets/worker_peers" = {
      mode = "0600";
      user = "gradient-worker";
      group = "gradient-worker";
      text = "*:C9ve6tvVONhtbRzFks56HQlYQotlRmXel/5NFLk/HjbSFGc+IZjCGfxegW2NKpY5";
    };

    security.pam.services.sshd.allowNullPassword = true;
    services = {
      gradient = {
        enable = true;
        reverseProxy.nginx.enable = true;
        useTls = false;
        configurePostgres = true;
        domain = "gradient.local";
        # The frontend is served by `pnpm run serve` on the host; the VM only provides the API.
        frontend.enable = false;
        proto.public = true;
        jwtSecretFile = toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4f8905623866edb2b344bd15de52ccece395b3546e2f00644eb2679cf7bdaa156fd75cc5f47c34448cba19d903e68015b1ad3c8e9d04862de0a2c525b6676779012919fa9551c4746f9323ab207aedae86c28ada67c901cae821eef97b69ca4ebe1260de31add34d8265f17d9c547e3bbabe284d9cadcc22063ee625b104592403368090642a41967f8ada5791cb09703d0762a3175d0fe06ec37822e9e41d0a623a6349901749673735fdb94f2c268ac08a24216efb058feced6e785f34185a");
        cryptSecretFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK");

        state = {
          users.admin = {
            email = "admin@gradient.local";
            password_file = toString adminPwHash;
            email_verified = true;
            superuser = true;
          };

          organizations.testorg = {
            display_name = "MyOrganization";
            description = "My Test Organization";
            private_key_file = toString orgSshKey;
            public = true;
            created_by = "admin";
          };

          projects.testproject = {
            organization = "testorg";
            display_name = "MyProject";
            description = "Just a test";
            repository = "git://server/test";
            wildcard = "packages.*.buildWait5Sec,packages.*.deployment";
            keep_evaluations = 10;
            created_by = "admin";
            triggers = [
              {
                type = "polling";
                config = { interval_secs = 30; };
              }
            ];
          };

          caches.testcache = {
            display_name = "MyCache";
            signing_key_file = toString cacheSigningKey;
            organizations = [ "testorg" ];
            public = true;
            created_by = "admin";
          };

          workers.devworker = {
            worker_id = "a0000000-0000-0000-0000-000000000001";
            organizations = [ "testorg" ];
            token_file = toString workerToken;
            display_name = "Dev Worker";
            created_by = "admin";
          };
        };

        worker = {
          enable = true;
          serverUrl = "ws://gradient.local/proto";
          peersFile = "/etc/gradient/secrets/worker_peers";
          capabilities = {
            eval = true;
            build = true;
          };
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

      gitDaemon = {
        enable = true;
        basePath = "/var/lib/git/";
        exportAll = true;
        options = "--enable=receive-pack";
      };

      openssh = {
        enable = true;
        settings = {
          PermitRootLogin = "yes";
          PermitEmptyPasswords = "yes";
        };
      };
    };

    # Allow git-daemon (runs as nobody) to access repos owned by other users.
    environment.etc."gitconfig".text = ''
      [safe]
        directory = *
    '';
  };
}
