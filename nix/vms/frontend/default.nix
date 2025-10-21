/*
 * SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, lib, ... }: {
  name = "development-frontend";
  testScript = { nodes, ... }: ''
    start_all()
    server.wait_for_unit("gradient-server.service")
    server.wait_for_unit("git-daemon.service")

    server.succeed("${lib.getExe pkgs.curl} http://gradient.local/api/v1/health -i --fail")

    server.succeed("""
        ${lib.getExe pkgs.curl} \
        -X POST \
        -H "Content-Type: application/json" \
        -d '{"username": "tes", "name": "Test User", "email": "test@localhost.local", "password": "Gradient123!"}' \
        http://gradient.local/api/v1/auth/basic/register
    """)

    token = server.succeed("""
      ${lib.getExe pkgs.curl} \
        -X POST \
        -H "Content-Type: application/json" \
        -d '{"loginname": "tes", "password": "Gradient123!"}' \
        http://gradient.local/api/v1/auth/basic/login \
        | ${lib.getExe pkgs.jq} -rj '.message'
    """)

    print(f"Got Token: {token}")

    server.succeed("${lib.getExe pkgs.gradient-cli} config Server http://gradient.local")
    server.succeed("${lib.getExe pkgs.gradient-cli} config AuthToken ACCESS_TOKEN".replace("ACCESS_TOKEN", token))
    server.succeed("${lib.getExe pkgs.gradient-cli} organization create --name testorg --display-name MyOrganization --description 'My Test Organization'")

    print("=== Adding Server ===")
    org_pub_key = server.succeed("${lib.getExe pkgs.gradient-cli} organization ssh show")[12:].strip()
    print(f"Got Organization Public Key: {org_pub_key}")

    server.succeed(f"echo '{org_pub_key}' > /home/builder/.ssh/authorized_keys")
    server.succeed("chown builder:users /home/builder/.ssh/authorized_keys")
    server.succeed("chmod 600 /home/builder/.ssh/authorized_keys")

    server.succeed("${lib.getExe pkgs.gradient-cli} server create --name testserver --display-name MyServer --host localhost --port 22 --ssh-user builder --architectures x86_64-linux --features big-parallel")

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
    server.succeed("${lib.getExe pkgs.gradient-cli} project create --name testproject --display-name MyProject --description 'Just a test' --repository git://server/test --evaluation-wildcard packages.*.buildWait5Sec,packages.*.deployment")
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
    virtualisation.forwardPorts = [
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

    nix.settings = {
      substituters = lib.mkForce [ ];
      trusted-users = [ "builder" ];
    };

    users.users.builder = {
      isNormalUser = true;
      group = "users";
    };

    systemd.tmpfiles.rules = [
      "d /home/builder/.ssh 0700 builder users"
      "d /var/lib/git 0755 git git"
      "L+ /var/lib/git/flake.nix 0755 git git - ${./flake_repository.nix}"
    ];

    security.pam.services.sshd.allowNullPassword = true;
    services = {
      gradient = {
        enable = true;
        serveCache = true;
        configureNginx = true;
        configurePostgres = true;
        domain = "gradient.local";
        jwtSecretFile = toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4f8905623866edb2b344bd15de52ccece395b3546e2f00644eb2679cf7bdaa156fd75cc5f47c34448cba19d903e68015b1ad3c8e9d04862de0a2c525b6676779012919fa9551c4746f9323ab207aedae86c28ada67c901cae821eef97b69ca4ebe1260de31add34d8265f17d9c547e3bbabe284d9cadcc22063ee625b104592403368090642a41967f8ada5791cb09703d0762a3175d0fe06ec37822e9e41d0a623a6349901749673735fdb94f2c268ac08a24216efb058feced6e785f34185a");
        cryptSecretFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK");
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
          # rust lib ssh2 requires one of the following Message Authentication Codes:
          # hmac-sha2-256,hmac-sha2-512,hmac-sha1,hmac-sha1-96,hmac-md5,hmac-md5-96,hmac-ripemd160,hmac-ripemd160@openssh.com
          Macs = [
            "hmac-sha2-512"
          ];
        };
      };
    };
  };
}
