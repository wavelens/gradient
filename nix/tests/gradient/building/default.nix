/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ self, pkgs, ... }: {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-building";
    globalTimeout = 960;

    defaults = {
      networking.firewall.enable = false;
      documentation.enable = false;
      nix.settings.substituters = lib.mkForce [ ];
      virtualisation = {
        cores = 8;
        memorySize = 4096;
        msize = 65536;
        writableStore = true;
      };
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
            cryptSecretFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK");
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
              logging_collector = true;
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

        nix.settings = {
          max-jobs = 0;
        };

        systemd.tmpfiles.rules = [
          "d /var/lib/git 0755 git git"
          "L+ /var/lib/git/flake.nix 0755 git git - ${./flake_repository.nix}"
          "L+ /var/lib/git/flake.lock 0755 git git - ${./flake_repository.lock}"
          "L+ /var/lib/git/build-test.nix 0755 git git - ${./build-test_repository.nix}"
        ];

        environment = {
          variables.TEST_PKGS = [
            self.inputs.nixpkgs
          ];

          systemPackages = with pkgs; [
            coreutils
            stdenv
            binutils
            busybox
          ];
        };
      };

      builder = { config, pkgs, lib, ... }: {
        imports = [ ../../../modules/gradient-worker.nix ];

        environment.variables.TEST_PKGS = [ self.inputs.nixpkgs ];

        nix.settings = {
          trusted-users = [
            "root"
            "@wheel"
          ];
        };

        services.gradient.worker = {
          enable = true;
          serverUrl = "ws://server/proto";
          capabilities = {
            eval = true;
            build = true;
          };
        };
      };
    };

    interactive.nodes = {
      server = import ../../modules/debug-host.nix;
    };

    testScript = { nodes, ... }:
      ''
      import json

      start_all()

      server.wait_for_unit("gradient-server.service")
      server.sleep(5)
      server.wait_for_unit("git-daemon.service")

      # Let the builder start to generate its persistent worker UUID, then stop it.
      # The worker will initially connect in open mode (no registrations exist yet).
      builder.wait_for_unit("gradient-worker.service")
      builder.succeed("systemctl restart nix-daemon.service")

      print(server.succeed("cat /etc/nix/nix.conf"))
      print(builder.succeed("cat /etc/nix/nix.conf"))

      # Stop the worker — we need to register it before it can authenticate
      builder.succeed("systemctl stop gradient-worker")

      # Test health endpoint
      server.succeed("${lib.getExe pkgs.curl} http://gradient.local/api/v1/health -i --fail")

      # Register user and login
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

      # Configure CLI
      server.succeed("${lib.getExe pkgs.gradient-cli} config Server http://gradient.local")
      server.succeed("${lib.getExe pkgs.gradient-cli} config AuthToken ACCESS_TOKEN".replace("ACCESS_TOKEN", token))

      print(server.succeed("${lib.getExe pkgs.gradient-cli} status"))
      print(server.succeed("${lib.getExe pkgs.gradient-cli} info"))

      # Create organization
      print("=== Testing Organization Commands ===")
      server.succeed("${lib.getExe pkgs.gradient-cli} organization create --name testorg --display-name MyOrganization --description 'My Test Organization'")
      print(server.succeed("${lib.getExe pkgs.gradient-cli} organization show"))

      # Organization SSH
      org_pub_key = server.succeed("${lib.getExe pkgs.gradient-cli} organization ssh show")[12:].strip()
      print(f"Got Organization Public Key: {org_pub_key}")

      # Cache commands
      server.succeed("${lib.getExe pkgs.gradient-cli} cache create --name testcache --display-name 'Test Cache' --description 'Test cache description' --priority 10")
      server.succeed("${lib.getExe pkgs.gradient-cli} organization cache add testcache")

      # ── Worker Authentication ──────────────────────────────────────────────────

      # Read the worker's persistent UUID (generated on first boot)
      worker_uuid = builder.succeed("cat /var/lib/gradient-worker/worker-id").strip()
      print(f"Worker UUID: {worker_uuid}")

      # Register the worker with the organization via API
      register_result = json.loads(server.succeed(f"""
        ${lib.getExe pkgs.curl} \
          -X POST \
          -H "Authorization: Bearer {token}" \
          -H "Content-Type: application/json" \
          -d '{{"worker_id": "{worker_uuid}"}}' \
          http://gradient.local/api/v1/orgs/testorg/workers
      """))

      assert register_result.get("error") == False, f"Worker registration failed: {register_result}"
      peer_id = register_result["message"]["peer_id"]
      worker_token = register_result["message"]["token"]
      print(f"Registered worker: peer_id={peer_id}")

      # Verify worker appears in the list
      workers_list = json.loads(server.succeed(f"""
        ${lib.getExe pkgs.curl} \
          -H "Authorization: Bearer {token}" \
          http://gradient.local/api/v1/orgs/testorg/workers
      """))
      assert any(w.get("worker_id") == worker_uuid for w in workers_list["message"]), \
          "Registered worker not found in list"

      # ── Sub-test: Worker rejected without token ────────────────────────────────

      # Restart worker WITHOUT a peers file. Since registered_peers is now non-empty
      # (we just registered it), the server will challenge and the worker has no tokens
      # to respond with → Reject(401).
      print("=== Sub-test: Worker auth rejection (no token) ===")
      builder.succeed("journalctl -u gradient-worker --rotate --vacuum-time=1s 2>/dev/null || true")
      builder.succeed("systemctl start gradient-worker")
      builder.sleep(15)
      reject_logs = builder.succeed("journalctl -u gradient-worker --no-pager -n 100")
      print(reject_logs)
      assert "server rejected connection (code 401)" in reject_logs, \
          f"Expected 401 rejection without token, but logs show:\n{reject_logs[-500:]}"
      print("=== Worker correctly rejected without token ===")

      # ── Sub-test: Worker authenticates with token ──────────────────────────────

      # Write the peers file and configure the worker to use it
      print("=== Sub-test: Worker auth success (with token) ===")
      builder.succeed("systemctl stop gradient-worker")
      builder.succeed(f"echo '{peer_id}:{worker_token}' > /tmp/worker-peers")
      builder.succeed("chmod 600 /tmp/worker-peers")
      builder.succeed("mkdir -p /etc/systemd/system/gradient-worker.service.d")
      builder.succeed("""echo '[Service]
      Environment=GRADIENT_WORKER_PEERS_FILE=/tmp/worker-peers' > /etc/systemd/system/gradient-worker.service.d/peers.conf""")
      builder.succeed("systemctl daemon-reload")
      builder.succeed("journalctl -u gradient-worker --rotate --vacuum-time=1s 2>/dev/null || true")
      builder.succeed("systemctl start gradient-worker")
      builder.sleep(10)
      auth_logs = builder.succeed("journalctl -u gradient-worker --no-pager -n 100")
      print(auth_logs)
      assert "handshake successful" in auth_logs, \
          f"Expected successful handshake, but logs show:\n{auth_logs[-500:]}"
      print("=== Worker authenticated successfully ===")

      # ── Git repository setup ───────────────────────────────────────────────────

      server.succeed("${lib.getExe pkgs.git} config --global --add safe.directory '*'")
      server.succeed("${lib.getExe pkgs.git} config --global init.defaultBranch main")
      server.succeed("${lib.getExe pkgs.git} config --global user.email 'nixos@localhost'")
      server.succeed("${lib.getExe pkgs.git} config --global user.name 'NixOS test'")

      server.succeed("${lib.getExe pkgs.git} init /var/lib/git/test")
      server.succeed("cp /var/lib/git/{,test/}flake.nix")
      server.succeed("cp /var/lib/git/{,test/}flake.lock")
      server.succeed("cp /var/lib/git/{,test/}build-test.nix")

      server.succeed("sed -i 's#\\[nixpkgs\\]#${self.inputs.nixpkgs}#g' /var/lib/git/test/flake.nix")
      server.succeed("sed -i 's#\\[nixpkgs\\]#${self.inputs.nixpkgs}#g' /var/lib/git/test/flake.lock")

      nixpkgs_hash = server.succeed("${lib.getExe pkgs.nix} hash path ${self.inputs.nixpkgs}").strip()
      server.succeed(f"sed -i 's#\\[hash\\]#{nixpkgs_hash}#g' /var/lib/git/test/flake.lock")

      server.succeed("chown git:git -R /var/lib/git/test")
      server.succeed("${lib.getExe pkgs.git} -C /var/lib/git/test add flake.nix")
      server.succeed("${lib.getExe pkgs.git} -C /var/lib/git/test add flake.lock")
      server.succeed("${lib.getExe pkgs.git} -C /var/lib/git/test add build-test.nix")
      server.succeed("${lib.getExe pkgs.git} -C /var/lib/git/test commit -m 'Initial commit'")

      server.succeed("${lib.getExe pkgs.git} clone git://localhost/test test")
      print(server.succeed("${lib.getExe pkgs.git} ls-remote git://server/test"))

      # ── Project creation and build ─────────────────────────────────────────────

      print("=== Testing Project Commands ===")
      server.succeed("${lib.getExe pkgs.gradient-cli} project create --name testproject --display-name MyProject --description 'Just a test' --repository git://server/test --evaluation-wildcard packages.*.default")
      print(server.succeed("${lib.getExe pkgs.gradient-cli} project list"))
      print(server.succeed("${lib.getExe pkgs.gradient-cli} project show"))

      # Test git repository connectivity
      print(server.succeed(f"""
        ${lib.getExe pkgs.curl} -i --fail \
          -X POST \
          -H "Authorization: Bearer {token}" \
          http://gradient.local/api/v1/projects/testorg/testproject/check-repository
      """))

      # Wait for evaluation and build to complete
      builder.sleep(150)
      print(server.succeed("su postgres -c 'psql -U postgres -d gradient -c \"SELECT * FROM build;\"'"))
      print(server.succeed("su postgres -c 'psql -U postgres -d gradient -c \"SELECT * FROM derivation_dependency;\"'"))
      builder.sleep(470)

      project_output = server.succeed("${lib.getExe pkgs.gradient-cli} project show")
      print(project_output)

      if "No builds." in project_output:
          raise Exception("Test failed: Evaluation shows 'No builds.' indicating failure")

      if not "Completed" in project_output:
          raise Exception("Test failed: Evaluation did not complete successfully")

      print("=== All Tests Completed Successfully ===")
      '';
  });
}
