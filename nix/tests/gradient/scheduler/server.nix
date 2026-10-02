/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

# Every task is holding one trigger that is never firing.
# Each phase is starting its own evaluation through the API.
{ lib, pkgs, storeSpec, upstream, specNames, workerToken, upstreamPeers }:
{ ... }:
{
  imports = [ ../../../modules/gradient.nix ];

  environment = {
    systemPackages = with pkgs; [ curl git jq ];

    etc = {
      "gradient/secrets/admin_password" = {
        mode = "0600";
        user = "gradient";
        group = "gradient";
        text = "$argon2id$v=19$m=4096,t=3,p=1$c29tZXNhbHQxMjM0NQ$hIKBEy9SOWlnAlcwUv2PLPBdsMkKhVlCyjTxaWIK+v4";
      };

      "gradient/secrets/corp_ssh_key" = {
        mode = "0600";
        user = "gradient";
        group = "gradient";
        text = ''
          -----BEGIN OPENSSH PRIVATE KEY-----
          b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
          QyNTUxOQAAACDle/PUDDuuI9h8+ViFyHMQjqARSRhLJcYKnay7MrflOgAAAJALQNCyC0DQ
          sgAAAAtzc2gtZWQyNTUxOQAAACDle/PUDDuuI9h8+ViFyHMQjqARSRhLJcYKnay7MrflOg
          AAAEAROowXB/e8+691yZgfHOASTPVyIM2Hx7U9RpmAtUda++V789QMO64j2Hz5WIXIcxCO
          oBFJGEslxgqdrLsyt+U6AAAABm5vbmFtZQECAwQFBgc=
          -----END OPENSSH PRIVATE KEY-----
        '';
      };

      "gradient/secrets/main_cache_key" = {
        mode = "0600";
        user = "gradient";
        group = "gradient";
        text = "22yRW7p/hxuPRWJh9pcfGH0oXPk2MFUuG0wIA1rfq1BvDbvMqzMZS+er/BE8ucbxNSG5KZ8B0ELO4TJal8mZlw==";
      };

      "gradient/secrets/worker_token" = {
        mode = "0600";
        user = "gradient";
        group = "gradient";
        text = workerToken;
      };

      "gitconfig".text = ''
        [safe]
          directory = *
        [init]
          defaultBranch = main
      '';
    };
  };

  networking.hosts."127.0.0.1" = [ "gradient.local" ];

  services = {
    gradient = {
      enable = true;
      reverseProxy.nginx.enable = true;
      postgres.enable = true;
      postgres.sharedBuffers = "128MB";
      domain = "gradient.local";
      proto.public = true;
      secrets.jwtFile = toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4f8905623866edb2b344bd15de52ccece395b3546e2f00644eb2679cf7bdaa156fd75cc5f47c34448cba19d903e68015b1ad3c8e9d04862de0a2c525b6676779012919fa9551c4746f9323ab207aedae86c28ada67c901cae821eef97b69ca4ebe1260de31add34d8265f17d9c547e3bbabe284d9cadcc22063ee625b104592403368090642a41967f8ada5791cb09703d0762a3175d0fe06ec37822e9e41d0a623a6349901749673735fdb94f2c268ac08a24216efb058feced6e785f34185a");
      secrets.cryptFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK");
      log.level.default = "info";
      proto.workerHeartbeatTimeoutSecs = 30;

      state = {
        users.admin = {
          email = "admin@example.com";
          password_file = "/etc/gradient/secrets/admin_password";
          superuser = true;
        };

        projects.project = {
          private_key_file = "/etc/gradient/secrets/corp_ssh_key";
          created_by = "admin";
        };

        tasks = lib.genAttrs specNames (name: {
          project = "project";
          repository = "git://server/${name}";
          created_by = "admin";
          triggers = [{
            type = "polling";
            active = false;
            config.interval_secs = 3600;
          }];
        });

        caches.main = {
          signing_key_file = "/etc/gradient/secrets/main_cache_key";
          projects = [ "project" ];
          public = true;
          created_by = "admin";
          upstream_caches = [{
            type = "external";
            display_name = "spec-upstream";
            url = "http://server/upstream";
            public_key = storeSpec.upstreamPublicKey;
          }];
        };

        workers = lib.mapAttrs (_: id: {
          worker_id = id;
          projects = [ "project" ];
          token_file = "/etc/gradient/secrets/worker_token";
          created_by = "admin";
        }) upstreamPeers;
      };
    };

    nginx.virtualHosts."gradient.local" = {
      enableACME = lib.mkForce false;
      forceSSL = lib.mkForce false;
      locations."/upstream/" = {
        alias = "${upstream}/";
        extraConfig = "autoindex off;";
      };
    };

    postgresql = {
      package = pkgs.postgresql_18;
      enableTCPIP = true;
      authentication = ''
        host all all 0.0.0.0/0 trust
        host all all ::0/0 trust
      '';
    };

    gitDaemon = {
      enable = true;
      basePath = "/var/lib/git/";
      exportAll = true;
    };
  };

  systemd.tmpfiles.rules = [ "d /var/lib/git 0755 git git" ];
}
