/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }: {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-frontend";
    globalTimeout = 600;
    skipTypeCheck = true;
    extraPythonPackages = ps: with ps; [ selenium ];

    defaults = {
      networking.firewall.enable = false;
      documentation.enable = false;
      nix.settings.substituters = lib.mkForce [ ];
      virtualisation = {
        writableStore = true;
        memorySize = 4096;
        diskSize = 4096;
      };
    };

    nodes = {
      machine = { config, pkgs, lib, ... }: {
        imports = [
          ../../../modules/gradient.nix
        ];

        networking.hosts = {
          "127.0.0.1" = [ "gradient.local" ];
        };

        virtualisation.forwardPorts = [
          {
            from = "host";
            host.port = 3000;
            guest.port = 3000;
          }
          {
            from = "host";
            host.port = 4444;
            guest.port = 4444;
          }
        ];

        nix.settings.max-jobs = 0;
        services = {
          gradient = {
            enable = true;
            frontend.enable = true;
            serveCache = true;
            configureNginx = true;
            configurePostgres = true;
            domain = "gradient.local";
            jwtSecretFile = toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4f8905623866edb2b344bd15de52ccece395b3546e2f00644eb2679cf7bdaa156fd75cc5f47c34448cba19d903e68015b1ad3c8e9d04862de0a2c525b6676779012919fa9551c4746f9323ab207aedae86c28ada67c901cae821eef97b69ca4ebe1260de31add34d8265f17d9c547e3bbabe284d9cadcc22063ee625b104592403368090642a41967f8ada5791cb09703d0762a3175d0fe06ec37822e9e41d0a623a6349901749673735fdb94f2c268ac08a24216efb058feced6e785f34185a");
            cryptSecretFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK");
            settings = {
              logLevel = "debug";
              enableRegistration = true;
            };
          };

          nginx.virtualHosts."gradient.local" = {
            enableACME = lib.mkForce false;
            forceSSL = lib.mkForce false;
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
        };

        environment.systemPackages = with pkgs; [
          ungoogled-chromium
          chromedriver
          curl
          jq
          xvfb-run
        ];

        systemd.services.chromedriver = {
          description = "ChromeDriver for Selenium tests";
          after = [ "network.target" ];
          wantedBy = [ "multi-user.target" ];
          serviceConfig = {
            ExecStart = "${lib.getExe pkgs.chromedriver} --port=4444 --whitelisted-ips=";
            Restart = "on-failure";
            Type = "simple";
          };
        };
      };
    };

    interactive.nodes = {
      machine = import ../../modules/debug-host.nix;
    };

    testScript = ''
      import os
      from selenium import webdriver

      os.environ["PATH"] += os.pathsep + "${pkgs.chromedriver}/bin"

      start_all()

      machine.wait_for_unit("gradient-server.service")
      machine.wait_for_unit("chromedriver.service")

      options = webdriver.ChromeOptions()
      options.binary_location = "${lib.getExe pkgs.ungoogled-chromium}"
      options.add_argument("--single-process")
      options.add_argument("--headless=new")
      options.add_argument("--no-sandbox")
      options.add_argument("--disable-setuid-sandbox")
      options.add_argument("--disable-gpu")
      options.add_argument("--disk-cache-size=0")
      options.add_argument("--disable-dev-shm-usage")
      options.add_argument("--disable-software-rasterizer")
      options.add_argument("--disable-background-networking")
      options.add_argument("--disable-sync")
      options.add_argument("--metrics-recording-only")
      options.add_argument("--no-first-run")
      options.add_argument("--disable-extensions")
      options.add_argument("--disable-features=VizDisplayCompositor")
      options.add_argument("--host-resolver-rules=MAP gradient.local 127.0.0.1")

      driver = webdriver.Remote(command_executor='http://localhost:4444', options=options)


      ${builtins.readFile ./test-login.py}
    '';
  });
}
