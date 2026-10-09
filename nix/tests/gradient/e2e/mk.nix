/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ self, pkgs, topology }: let
  testStore = import ../../../scripts/store.nix {
    inherit pkgs;
    skipDirectories = false;
  };

  workerToken = "C9ve6tvVONhtbRzFks56HQlYQotlRmXel/5NFLk/HjbSFGc+IZjCGfxegW2NKpY5";
  workerIds = {
    builder = "a0000000-0000-0000-0000-000000000001";
    builder2 = "a0000000-0000-0000-0000-000000000002";
  };

  builderModule = { config, pkgs, lib, ... }: {
    imports = [ ../../../modules/gradient-worker.nix ];

    virtualisation.additionalPaths = [ testStore ];

    nix.settings = {
      trusted-users = [
        "root"
        "@wheel"
      ];

      # One job per core, here and in the worker's build.maxConcurrent.
      # The builder kernel-panicked on OOM (`compulsory panic_on_oom`) with 2048 MB and four cores.
      # 8 jobs did that, and so did the worker's default of 32 builds over 304 nix-daemon connections.
      max-jobs = lib.mkForce 4;
    };

    services.gradient.worker = {
      enable = true;
      build = {
        maxConcurrent = 4;
        metrics = true;
      };
      capabilities = {
        eval  = true;
        build = true;
      };
    };
  };

  topo = topology {
    inherit pkgs;
    inherit (pkgs) lib;
    token = workerToken;
    workers = pkgs.lib.mapAttrs (_: id: { inherit id; module = builderModule; }) workerIds;
  };
in
assert import ../../harness/contract.nix { inherit (pkgs) lib; topology = topo; workers = workerIds; };
pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
  name = "gradient-e2e";
  globalTimeout = 5400;

  defaults = {
    networking.firewall.enable = false;
    virtualisation = {
      cores = 4;
      memorySize = 2048;
      diskSize = 8192;
      writableStore = true;
    };
    documentation.enable = false;
    nix.settings.max-jobs = 0;
  };

  nodes = {
    server = { config, pkgs, lib, ... }: {
      imports = [
        ../../../modules/gradient.nix
      ];

      nix.settings.substituters = lib.mkForce [ ];

      virtualisation.additionalPaths = [ pkgs.busybox.debug ];

      environment = {
        variables.TEST_PKGS = [ self.inputs.nixpkgs ];
        systemPackages = with pkgs; [
          binutils
          busybox
          coreutils
          hello
          stdenv
        ];

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

          "gradient/secrets/upstream_key" = {
            mode = "0600";
            text = "file-upstream-1:eZPukjHYgRpJ+hLlnUgG+qpi/k4QMTr3bd4ftngZJwkIXutyHrlDclZGwczy+IKCAt82HkKrDtM16u9HR9uzkQ==";
          };

          "gradient/secrets/worker_token" = {
            mode = "0600";
            user = "gradient";
            group = "gradient";
            text = workerToken;
          };
        };
      };

      networking.hosts = {
        # `hook.local` is phase 14's webhook target, a name and not `127.0.0.1`.
        # `validate_webhook_url` is rejecting every loopback and private literal.
        # Every address in a NixOS VM network is one of those.
        "127.0.0.1" = [ "gradient.local" "hook.local" ];
      };

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
          log.level.default = "debug";
          gc = {
            intervalSecs = 20;
            narTtlHours = 1;
          };
          state = {
            users = {
              admin = {
                email = "admin@example.com";
                password_file = "/etc/gradient/secrets/admin_password";
                superuser = true;
              };
            };

            projects = {
              project = {
                private_key_file = "/etc/gradient/secrets/corp_ssh_key";
                created_by = "admin";
              };
            };

            tasks = {
              task = {
                project = "project";
                repository = "git://server/test";
                created_by = "admin";
                triggers = [
                  {
                    type = "polling";
                    config = { interval_secs = 10; };
                  }
                ];
                actions = [
                  {
                    name = "restart-probe";
                    type = "send_web_request";
                    events = [ "evaluation.approval_granted" ];
                    config = { url = "http://hook.local:8099/hook"; };
                  }
                ];
              };

              task2 = {
                project = "project";
                repository = "git://server/test";
                created_by = "admin";
                triggers = [
                  {
                    type = "polling";
                    config = { interval_secs = 10; };
                  }
                ];
              };
            };

            caches = {
              main = {
                signing_key_file = "/etc/gradient/secrets/main_cache_key";
                projects = [ "project" ];
                public = true;
                created_by = "admin";
                upstream_caches = [{
                  type = "external";
                  display_name = "file-upstream";
                  url = "http://server/upstream";
                  public_key = "file-upstream-1:CF7rch65Q3JWRsHM8viCggLfNh5Cqw7TNervR0fbs5E=";
                }];
              };
            };

            workers = lib.mapAttrs (name: id: {
              worker_id = id;
              projects = [ "project" ];
              token_file = "/etc/gradient/secrets/worker_token";
              created_by = "admin";
              url = (topo.upstreamUrls or { }).${name} or null;
            }) topo.upstreamPeers;
          };
        };

        nginx.virtualHosts."gradient.local" = {
          enableACME = lib.mkForce false;
          forceSSL = lib.mkForce false;
          locations."/upstream/" = {
            alias = "/srv/upstream/";
            extraConfig = "autoindex off;";
          };
        };

        postgresql = {
          package = pkgs.postgresql_18;
          enableTCPIP = true;
          authentication = ''
            host  all      all     0.0.0.0/0      trust
            host all       all     ::0/0        trust
          '';

          settings = {
            logging_collector = true;
            log_destination = lib.mkForce "syslog";
            shared_preload_libraries = "pg_stat_statements";
            "pg_stat_statements.max" = 10000;
          };
        };

        gitDaemon = {
          enable = true;
          basePath = "/var/lib/git/";
          exportAll = true;
          options = "--enable=receive-pack";
        };
      };

      environment.etc."gitconfig".text = ''
        [safe]
          directory = *
      '';

      systemd.tmpfiles.rules = [
        "d /var/lib/git 0755 git git"
        "d /srv/upstream 0755 nginx nginx"
        "L+ /var/lib/git/flake.nix 0755 git git - ${./flake_repository.nix}"
        "L+ /var/lib/git/flake.lock 0755 git git - ${./flake_repository.lock}"
        "L+ /var/lib/git/flake-busywrap.nix 0755 git git - ${./flake_repository_busywrap.nix}"
        "L+ /var/lib/git/flake-spin.nix 0755 git git - ${./flake_repository_spin.nix}"
        "L+ /var/lib/git/flake-slow.nix 0755 git git - ${./flake_repository_slow.nix}"
      ];
    };

    client = { config, pkgs, lib, ... }: {
      environment.variables.TEST_PKGS = [ self.inputs.nixpkgs ];
      nix.settings = {
        substituters = lib.mkForce [ "http://server/cache/main" ];
        trusted-public-keys = lib.mkForce [ "gradient.local-main:bw27zKszGUvnq/wRPLnG8TUhuSmfAdBCzuEyWpfJmZc=" ];
      };
    };
  } // topo.nodes;

  interactive.nodes = lib.genAttrs ([ "server" "client" ] ++ lib.attrNames topo.nodes) (_: import ../../modules/debug-host.nix);

  testScript = { nodes, ... }:
    ''
    import json
    import time

    GIT     = "${lib.getExe pkgs.git}"
    CURL    = "${lib.getExe pkgs.curl}"
    JQ      = "${lib.getExe pkgs.jq}"
    NIX     = "${lib.getExe pkgs.nix}"
    CLI     = "${lib.getExe pkgs.gradient-cli}"
    API     = "http://gradient.local/api/v1"
    CACHE   = "http://server/cache/main"
    ${import ../../harness/prelude.nix { inherit lib; topology = topo; }}

    def banner(msg):
        """Loud step header, easy to grep in CI output."""
        print(f"\n=== {msg} ===")

    def sql(query):
        server.succeed(f"cat > /tmp/q.sql <<'EOF'\n{query}\nEOF")
        return server.succeed("su postgres -c 'psql -v ON_ERROR_STOP=1 -d gradient -At -f /tmp/q.sql'").strip()

    def api_get(token, path):
        """GET ``API/<path>``, return the parsed `.message` field as text."""
        return server.succeed(
            f'{CURL} -sf -H "Authorization: Bearer {token}" "{API}/{path}"'
        )

    def blocking_shared_builds(evaluation):
        """The shared builds keeping `evaluation` in its build phase, bucketed by the
        terms of `gates_predicate`. A `Created` row's `fetchable` is false by
        definition, so a status histogram carries no information about WHY one
        is not queued; these booleans do. `probed` is not a gate on the row it
        is printed for: it is why the rows BELOW it are missing from this list,
        which is the one shape the others cannot show."""
        return sql(
            f"SELECT status::text || ' walked=' || walked::int::text"
            f"  || ' drv=' || drv::int::text"
            f"  || ' cache_available=' || cache_available::int::text"
            f"  || ' probed=' || probed::int::text"
            f"  || ' no_blocking_deps=' || no_blocking_deps::int::text"
            f"  || ' present=' || present::int::text"
            f"  || ' complete=' || complete::int::text"
            f"  || ' count=' || count(*)::text"
            f" FROM (SELECT db.status, w.walked, db.cache_available, db.probed,"
            f"              EXISTS (SELECT 1 FROM cached_path cp"
            f"                      WHERE cp.hash = w.hash AND cp.file_hash IS NOT NULL) AS drv,"
            f"              db.blocking_deps = 0 AS no_blocking_deps,"
            f"              db.missing_runtime_deps = 0 AS complete,"
            f"              NOT EXISTS (SELECT 1 FROM derivation_output o"
            f"                          LEFT JOIN cached_path cp"
            f"                            ON cp.hash = o.hash AND cp.file_hash IS NOT NULL"
            f"                          WHERE o.derivation = db.derivation"
            f"                            AND cp.hash IS NULL) AS present"
            f"       FROM derivation_build db"
            f"       JOIN derivation w ON w.id = db.derivation"
            f"       JOIN build_job bj ON bj.derivation_build = db.id"
            f"       WHERE bj.evaluation = '{evaluation}'"
            f"         AND db.status IN (0, 1, 2, 8)"
            f"         AND (db.wanted OR db.status IN (1, 2))) g"
            f" GROUP BY status, walked, drv, cache_available, probed, no_blocking_deps, present, complete"
            f" ORDER BY 1;"
        )

    def blocking_reasons(evaluation):
        """Why each blocking shared build of `evaluation` still counts a blocking
        dependency, named on both sides. `blocking_deps` is a number; a hang needs
        the edge it stands for and the dependency's own status, complete closure and
        presence, which is what stops it being fetchable."""
        present = (
            "NOT EXISTS (SELECT 1 FROM derivation_output o"
            "            LEFT JOIN cached_path cp"
            "              ON cp.hash = o.hash AND cp.file_hash IS NOT NULL"
            "            WHERE o.derivation = dep.derivation AND cp.hash IS NULL)"
        )
        fetchable = f"dep.status IN (3, 7) AND dep.missing_runtime_deps = 0 AND {present}"
        return sql(
            f"SELECT d.name || ' [' || db.status::text || ']  needs  ' || dn.name"
            f"    || ' [' || coalesce(dep.status::text, 'no build row')"
            f"    || ' kind=' || e.kind::text"
            f"    || ' complete=' || coalesce((dep.missing_runtime_deps = 0)::int::text, '-')"
            f"    || ' present=' || coalesce(({present})::int::text, '-') || ']'"
            f" FROM derivation_build db"
            f" JOIN derivation d ON d.id = db.derivation"
            f" JOIN build_job bj ON bj.derivation_build = db.id"
            f" JOIN derivation_dependency e ON e.derivation = db.derivation"
            f" JOIN derivation dn ON dn.id = e.dependency"
            f" LEFT JOIN derivation_build dep ON dep.derivation = e.dependency"
            f" WHERE bj.evaluation = '{evaluation}'"
            f"   AND db.status IN (0, 1, 2, 8) AND (db.wanted OR db.status IN (1, 2))"
            f"   AND (dep.derivation IS NULL OR NOT ({fetchable}))"
            f" ORDER BY 1 LIMIT 40;"
        )

    def assert_no_server_error(j):
        """The lines a healthy run never writes, named rather than counted. A bare
        `needle in j` says a pool timed out somewhere in the last 900 s, which
        names neither the pool, the module nor how often; the slowest statements
        below name what was holding the connections."""
        hits = {}
        for needle in ("pool timed out", "graph call timed out", "graph writer unreachable",
                       "record transaction failed", "dropped as stale"):
            lines = [line[-300:] for line in j.splitlines() if needle in line]
            if lines:
                hits[needle] = lines
        if not hits:
            return

        report = "\n\n".join(
            f"{needle!r}, {len(lines)} lines:\n" + "\n".join(lines[:6])
            for needle, lines in hits.items()
        )
        refused = server.succeed(
            "journalctl --no-pager | grep -c 'too many clients already' || true"
        ).strip()
        sql("CREATE EXTENSION IF NOT EXISTS pg_stat_statements;")
        slowest = sql(
            "SELECT round(s.max_exec_time::numeric, 0) || ' ms max, ' || s.calls || ' calls: '"
            "    || regexp_replace(substring(s.query, 1, 160), '[[:space:]]+', ' ', 'g')"
            " FROM pg_stat_statements s JOIN pg_roles r ON r.oid = s.userid"
            " WHERE r.rolname <> 'postgres'"
            " ORDER BY s.max_exec_time DESC LIMIT 10;"
        )
        raise Exception(
            f"the server log carries what a healthy run does not:\n{report}\n\n"
            f"postgres refused a connection {refused} times, so a pool timeout above "
            f"is a busy pool and not a cluster at its ceiling\n\n"
            f"the server's slowest single executions:\n{slowest}"
        )

    def assert_no_server_panic(since_seconds=45):
        """Fail fast if gradient-server panicked since `since_seconds` ago."""
        j = server.succeed(
            f"journalctl -u gradient-server --no-pager --since='-{since_seconds}s' -n 200"
        )
        if "panicked" in j or "SIGABRT" in j:
            raise Exception(f"Gradient server crashed:\n{j[-2000:]}")
        return j

    LOCK_RACE_SH = """
    set -u
    D=/tmp/lockrace
    APID=""
    BPID=""

    cleanup() {
      exec 3>&-
      exec 4>&-
      if [ -n "$APID" ]; then kill $APID 2>/dev/null || true; fi
      if [ -n "$BPID" ]; then kill $BPID 2>/dev/null || true; fi
      mkdir -p $D
      printf '%s\\n' "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name LIKE 'lockrace_%';" > $D/kill.sql
      su postgres -c "psql -d gradient -At -q -f $D/kill.sql" >/dev/null 2>&1 || true
      rm -rf $D
    }
    trap cleanup EXIT INT TERM

    q() {
      printf '%s\\n' "$1" > $D/q.sql
      su postgres -c "psql -d gradient -At -q -f $D/q.sql"
    }

    wait_state() {
      i=0
      while [ $i -lt 60 ]; do
        if [ "$(q "SELECT count(*) FROM pg_stat_activity WHERE $1")" = "1" ]; then
          return 0
        fi
        i=$((i + 1))
        sleep 1
      done
      echo "LOCKRACE TIMEOUT: $2"
      echo "--- retire session ---"
      cat $D/a.out || true
      echo "--- mark session ---"
      cat $D/b.out || true
      q "SELECT pid, application_name, state, wait_event_type, wait_event, left(query, 90) FROM pg_stat_activity WHERE datname = 'gradient' ORDER BY pid"
      exit 1
    }

    send_a() { printf '%s\\n' "$1" >&3; }
    send_b() { printf '%s\\n' "$1" >&4; }

    fail() {
      echo "LOCKRACE: $1"
      echo "--- retire session ---"
      cat $D/a.out
      echo "--- mark session ---"
      cat $D/b.out
      exit 1
    }

    expect() {
      if [ "$(grep -cx "$2" $D/$1.out)" != "$3" ]; then
        fail "$4"
      fi
    }

    fixture() {
      q "INSERT INTO derivation (id, created_at, architecture, hash, name, prefer_local_build, allow_substitutes, is_fixed_output, walked) VALUES ('$1', now() AT TIME ZONE 'UTC', 'x86_64-linux', '$2', '$4', false, true, false, false); INSERT INTO derivation_build (id, derivation, status, cache_available, substituted, fetchable, blocking_deps, attempt, created_at, updated_at) VALUES (uuidv7(), '$1', 3, false, false, false, 0, 0, now() AT TIME ZONE 'UTC', now() AT TIME ZONE 'UTC'); INSERT INTO derivation_output (id, derivation, name, hash, package, is_cached, created_at) VALUES (uuidv7(), '$1', 'out', '$3', '$4-out', true, now() AT TIME ZONE 'UTC'); INSERT INTO cached_path (id, hash, package, file_hash, file_size, nar_size, nar_hash, confirmed, created_at) VALUES (uuidv7(), '$3', '$4-out', 'sha256:lockrace', 1, 1, 'sha256:lockrace', false, now() AT TIME ZONE 'UTC');"
    }

    retire() {
      send_a "BEGIN;"
      send_a "SELECT 1 FROM cached_path WHERE hash = ANY(ARRAY['$2']) ORDER BY hash FOR NO KEY UPDATE;"
      send_a "DELETE FROM cached_path WHERE hash = ANY(ARRAY['$2']);"
      send_a "SELECT 'held ' || fetchable::int FROM derivation_build WHERE derivation = ANY(ARRAY['$1']::uuid[]) ORDER BY derivation FOR NO KEY UPDATE;"
      wait_state "application_name = 'lockrace_a' AND state = 'idle in transaction' AND query LIKE '%fetchable::int%'" "the retire session never reached its held state"
      expect a "DELETE 1" "$3" "the retire deleted no row, its fixture was gone before the arm ran"
      expect a "held 0" "$3" "the retire held a fixture that was already fetchable: a consistency pass marked it between its insert and the retire"
    }

    mark() {
      printf '%s\\n' "UPDATE derivation_build db SET fetchable = true WHERE db.derivation = ANY(ARRAY['$1']::uuid[]) AND NOT db.fetchable AND (db.status IN (3, 7) AND (db.missing_runtime_deps = 0 AND (EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = db.derivation) AND NOT EXISTS (SELECT 1 FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash WHERE o.derivation = db.derivation AND cp.file_hash IS NULL)))) RETURNING 'marked ' || db.derivation;"
    }

    rm -rf $D
    mkdir -p $D
    mkfifo $D/a.in
    mkfifo $D/b.in
    su postgres -c "psql -d gradient -At" < $D/a.in > $D/a.out 2>&1 &
    APID=$!
    su postgres -c "psql -d gradient -At" < $D/b.in > $D/b.out 2>&1 &
    BPID=$!
    exec 3> $D/a.in
    exec 4> $D/b.in
    send_a "SET application_name = 'lockrace_a';"
    send_b "SET application_name = 'lockrace_b';"

    fixture "$LR_DRV" "$LR_DRV_HASH" "$LR_OUT" lockrace-probe
    retire "$LR_DRV" "$LR_OUT" 1

    send_b "BEGIN;"
    send_b "SELECT 1 FROM derivation_build WHERE derivation = ANY(ARRAY['$LR_DRV']::uuid[]) ORDER BY derivation FOR NO KEY UPDATE;"
    wait_state "application_name = 'lockrace_b' AND wait_event_type = 'Lock'" "the mark session never blocked on the shared build lock"
    echo "lockrace: the locked mark is blocked on the retire"

    send_a "COMMIT;"
    wait_state "application_name = 'lockrace_a' AND state = 'idle'" "the retire session never committed"

    send_b "$(mark "$LR_DRV")"
    send_b "COMMIT;"
    wait_state "application_name = 'lockrace_b' AND state = 'idle'" "the locked mark session never committed"

    if grep -q ERROR $D/a.out $D/b.out; then
      fail "a session reported an error in the locked arm"
    fi
    expect b "UPDATE 0" 1 "the locked mark wrote fetchable = true for a shared build whose only output the retire deleted: its snapshot was not ordered after that commit"
    echo "lockrace: the locked mark read the committed retire and wrote nothing"

    fixture "$LR_DRV2" "$LR_DRV2_HASH" "$LR_OUT2" lockrace-unlocked
    retire "$LR_DRV2" "$LR_OUT2" 2

    send_b "BEGIN;"
    send_b "$(mark "$LR_DRV2")"
    wait_state "application_name = 'lockrace_b' AND wait_event_type = 'Lock'" "the unlocked mark never blocked on the retire"
    send_a "COMMIT;"
    wait_state "application_name = 'lockrace_b' AND state = 'idle in transaction'" "the unlocked mark never resumed after the retire committed"
    send_b "COMMIT;"
    wait_state "application_name = 'lockrace_b' AND state = 'idle'" "the unlocked mark session never committed"

    if grep -q ERROR $D/a.out $D/b.out; then
      fail "a session reported an error in the unlocked arm"
    fi
    expect b "marked $LR_DRV2" 1 "the UNLOCKED mark did not write the stale fetchable = true this lock exists to prevent. The two arms now agree, which means taking the shared build lock in its own statement is no longer what makes the mark correct: re-derive the discipline in gradient_db::graph::can_start before trusting it"
    echo "lockrace: the unlocked mark wrote the stale fetchable = true from its pre-commit snapshot"
    """

    COUNTER_RACE_SH = """
    set -u
    D=/tmp/counterrace
    APID=""
    BPID=""

    cleanup() {
      exec 3>&-
      exec 4>&-
      if [ -n "$APID" ]; then kill $APID 2>/dev/null || true; fi
      if [ -n "$BPID" ]; then kill $BPID 2>/dev/null || true; fi
      mkdir -p $D
      printf '%s\\n' "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name LIKE 'counterrace_%';" > $D/kill.sql
      su postgres -c "psql -d gradient -At -q -f $D/kill.sql" >/dev/null 2>&1 || true
      rm -rf $D
    }
    trap cleanup EXIT INT TERM

    q() {
      printf '%s\\n' "$1" > $D/q.sql
      su postgres -c "psql -d gradient -At -q -v ON_ERROR_STOP=1 -f $D/q.sql"
    }

    wait_state() {
      i=0
      while [ $i -lt 60 ]; do
        if [ "$(q "SELECT count(*) FROM pg_stat_activity WHERE $1")" = "1" ]; then
          return 0
        fi
        i=$((i + 1))
        sleep 1
      done
      echo "COUNTERRACE TIMEOUT: $2"
      cat $D/a.out || true
      cat $D/b.out || true
      q "SELECT pid, application_name, state, wait_event_type, wait_event, left(query, 90) FROM pg_stat_activity WHERE datname = 'gradient' ORDER BY pid"
      exit 1
    }

    send_a() { printf '%s\\n' "$1" >&3; }
    send_b() { printf '%s\\n' "$1" >&4; }

    name() {
      echo "INSERT INTO build_job (id, evaluation, derivation, derivation_build, score, score_breakdown, created_at) SELECT uuidv7(), '$CR_EVAL', db.derivation, db.id, 0, '{}'::jsonb, now() AT TIME ZONE 'UTC' FROM derivation_build db WHERE db.id = '$1';"
    }

    rm -rf $D
    mkdir -p $D

    q "BEGIN; SELECT pg_advisory_xact_lock(640); UPDATE evaluation SET status = 3, waiting_reason = NULL WHERE id = '$CR_EVAL'; $(name "$CR_HOLD") WITH gone AS (DELETE FROM evaluation_shared_build_delta WHERE evaluation = '$CR_EVAL' RETURNING 1), c AS (SELECT count(bj.id)::int AS named, coalesce(sum(x.active), 0)::int AS active, coalesce(sum(x.failed), 0)::int AS failed, coalesce(sum(x.queued), 0)::int AS queued, coalesce(sum(x.building), 0)::int AS building FROM build_job bj JOIN derivation_build db ON db.id = bj.derivation_build CROSS JOIN LATERAL evaluation_shared_build_counts(db.status, db.wanted) x WHERE bj.evaluation = '$CR_EVAL') UPDATE evaluation e SET named_shared_builds = c.named, active_shared_builds = c.active, failed_shared_builds = c.failed, queued_shared_builds = c.queued, building_shared_builds = c.building FROM c WHERE e.id = '$CR_EVAL'; COMMIT;" >/dev/null

    mkfifo $D/a.in
    mkfifo $D/b.in
    su postgres -c "psql -d gradient -At" < $D/a.in > $D/a.out 2>&1 &
    APID=$!
    su postgres -c "psql -d gradient -At" < $D/b.in > $D/b.out 2>&1 &
    BPID=$!
    exec 3> $D/a.in
    exec 4> $D/b.in
    send_a "SET application_name = 'counterrace_a';"
    send_b "SET application_name = 'counterrace_b';"

    send_a "BEGIN;"
    send_a "UPDATE derivation_build SET status = 3 WHERE id = '$CR_BUILD';"
    wait_state "application_name = 'counterrace_a' AND state = 'idle in transaction'" "the move never held its row"
    send_b "BEGIN;"
    send_b "$(name "$CR_BUILD")"
    wait_state "application_name = 'counterrace_b' AND state = 'idle in transaction'" "the naming waited on the move: a trigger took a lock"
    send_b "COMMIT;"
    wait_state "application_name = 'counterrace_b' AND state = 'idle'" "the naming never committed"
    send_a "COMMIT;"
    wait_state "application_name = 'counterrace_a' AND state = 'idle'" "the move never committed"

    if grep -q ERROR $D/a.out $D/b.out; then
      echo "COUNTERRACE: a session reported an error"
      cat $D/a.out
      cat $D/b.out
      exit 1
    fi

    q "UPDATE derivation_build SET status = 3 WHERE id = '$CR_HOLD';" >/dev/null
    echo "counterrace: raced and released"
    """

    CLAIM_RACE_SH = """
    set -u
    D=/tmp/claimrace
    APID=""
    BPID=""

    cleanup() {
      exec 3>&-
      exec 4>&-
      if [ -n "$APID" ]; then kill $APID 2>/dev/null || true; fi
      if [ -n "$BPID" ]; then kill $BPID 2>/dev/null || true; fi
      mkdir -p $D
      printf '%s\\n' "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name LIKE 'claimrace_%';" > $D/kill.sql
      su postgres -c "psql -d gradient -At -q -f $D/kill.sql" >/dev/null 2>&1 || true
      rm -rf $D
    }
    trap cleanup EXIT INT TERM

    q() {
      printf '%s\\n' "$1" > $D/q.sql
      su postgres -c "psql -d gradient -At -q -v ON_ERROR_STOP=1 -f $D/q.sql"
    }

    wait_state() {
      i=0
      while [ $i -lt 60 ]; do
        if [ "$(q "SELECT count(*) FROM pg_stat_activity WHERE $1")" = "1" ]; then
          return 0
        fi
        i=$((i + 1))
        sleep 1
      done
      echo "CLAIMRACE TIMEOUT: $2"
      cat $D/a.out || true
      cat $D/b.out || true
      exit 1
    }

    claim() {
      echo "INSERT INTO dispatched_job (id, kind, evaluation_id, project, worker_id, job_id, score, queued_at, dispatched_at, score_breakdown, worker_context, job_context, created_at) SELECT uuidv7(), 1, '$CL_EVAL', '$CL_PROJECT', '$1', 'build:$CL_BUILD', 0, now() AT TIME ZONE 'UTC', now() AT TIME ZONE 'UTC', '{}'::jsonb, '{}'::jsonb, '{}'::jsonb, now() AT TIME ZONE 'UTC' WHERE EXISTS (SELECT 1 FROM derivation_build WHERE id = '$CL_BUILD' AND status = 1 AND cache_available = false) ON CONFLICT (job_id) WHERE finished_at IS NULL DO NOTHING;"
    }

    rm -rf $D
    mkdir -p $D
    mkfifo $D/a.in
    mkfifo $D/b.in
    su postgres -c "psql -d gradient -At" < $D/a.in > $D/a.out 2>&1 &
    APID=$!
    su postgres -c "psql -d gradient -At" < $D/b.in > $D/b.out 2>&1 &
    BPID=$!
    exec 3>$D/a.in
    exec 4>$D/b.in

    printf '%s\\n' "SET application_name = 'claimrace_a';" >&3
    printf '%s\\n' "SET application_name = 'claimrace_b';" >&4

    printf '%s\\n' "BEGIN;" >&3
    printf '%s\\n' "$(claim instance-a)" >&3
    wait_state "application_name = 'claimrace_a' AND state = 'idle in transaction'" "the first claim never inserted"
    printf '%s\\n' "BEGIN;" >&4
    printf '%s\\n' "$(claim instance-b)" >&4
    wait_state "application_name = 'claimrace_b' AND wait_event_type = 'Lock'" "the second claim did not wait on the open-row index"
    printf '%s\\n' "COMMIT;" >&3
    wait_state "application_name = 'claimrace_a' AND state = 'idle'" "the first claim never committed"
    wait_state "application_name = 'claimrace_b' AND state = 'idle in transaction'" "the second claim never finished"
    printf '%s\\n' "COMMIT;" >&4
    wait_state "application_name = 'claimrace_b' AND state = 'idle'" "the second claim never committed"

    if grep -q ERROR $D/a.out $D/b.out; then
      echo "CLAIMRACE: a session reported an error"
      cat $D/a.out
      cat $D/b.out
      exit 1
    fi

    echo "claimrace_a $(grep INSERT $D/a.out)"
    echo "claimrace_b $(grep INSERT $D/b.out)"
    """

    LOCK_GUARD_SH = r"""
    set -u
    D=/tmp/lockguard
    APID=""
    BPID=""
    P=00000000-0000-4000-8000-00000000000a
    DEP=00000000-0000-4000-8000-00000000000b
    Q=00000000-0000-4000-8000-00000000000c

    pg() { eval "$LG_PSQL"; }

    cleanup() {
      exec 3>&- 2>/dev/null
      exec 4>&- 2>/dev/null
      if [ -n "$APID" ]; then kill $APID 2>/dev/null || true; fi
      if [ -n "$BPID" ]; then kill $BPID 2>/dev/null || true; fi
      mkdir -p $D
      printf '%s\n' "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name LIKE 'lockguard_%';" | pg >/dev/null 2>&1 || true
      rm -rf $D
    }
    trap cleanup EXIT INT TERM

    q() { printf '%s\n' "SET search_path = lockguard, public;" "$1" | pg | grep -v '^SET$'; }

    fail() {
      echo "LOCKGUARD: $1"
      echo "--- session a ---"; cat $D/a.out || true
      echo "--- session b ---"; cat $D/b.out || true
      q "SELECT pid, application_name, state, wait_event_type, wait_event, left(query, 90) FROM pg_stat_activity WHERE application_name LIKE 'lockguard_%' ORDER BY pid"
      exit 1
    }

    wait_state() {
      i=0
      while [ $i -lt 60 ]; do
        if [ "$(q "SELECT count(*) FROM pg_stat_activity WHERE $1")" = "1" ]; then
          return 0
        fi
        i=$((i + 1))
        sleep 1
      done
      fail "timeout: $2"
    }

    stmt() { $LG_GATE --print "$1"; }

    bind() {
      s="$1"
      s="''${s//\$1/$2}"
      if [ $# -ge 3 ]; then s="''${s//\$2/$3}"; fi
      printf '%s;' "$s"
    }

    LOCK_SHARED_BUILDS=$(stmt LOCK_SHARED_BUILDS) || exit 1
    LOCK_SEED=$(stmt LOCK_SEED_SHARED_BUILDS) || exit 1
    CLOSURE_COMPLETE_AMONG=$(stmt CLOSURE_COMPLETE_AMONG) || exit 1
    LOCK_PATHS=$(stmt LOCK_CACHED_PATHS) || exit 1
    SEED=$(stmt SEED_MISSING_RUNTIME_DEPS) || exit 1
    RIPPLE=$(stmt RIPPLE_MISSING_RUNTIME_DEPS) || exit 1
    RECOUNT=$(stmt RECOUNT_MISSING_RUNTIME_DEPS) || exit 1

    send_a() { printf '%s\n' "$1" >&3; }
    send_b() { printf '%s\n' "$1" >&4; }
    idle_tx() { wait_state "application_name = 'lockguard_$1' AND state = 'idle in transaction'" "$2"; }
    idle() { wait_state "application_name = 'lockguard_$1' AND state = 'idle'" "$2"; }
    blocked() { wait_state "application_name = 'lockguard_$1' AND wait_event_type = 'Lock'" "$2"; }

    reset() {
      q "TRUNCATE derivation_build, derivation_dependency, derivation_output, cached_path;
         INSERT INTO derivation_build (id, derivation, created_at, updated_at)
           SELECT uuidv7(), d, now(), now() FROM unnest(ARRAY['$P', '$DEP', '$Q']::uuid[]) d;
         INSERT INTO derivation_output (id, derivation, name, hash, package, created_at)
           VALUES (uuidv7(), '$P', 'out', 'lgp', 'p', now()),
                  (uuidv7(), '$DEP', 'out', 'lgd', 'd', now()),
                  (uuidv7(), '$Q', 'out', 'lgq', 'q', now());
         INSERT INTO cached_path (id, hash, package, file_hash, created_at)
           VALUES (uuidv7(), 'lgp', 'p', 'sha256:p', now()), (uuidv7(), 'lgq', 'q', 'sha256:q', now());" >/dev/null
      if [ "$1" = present ]; then
        q "INSERT INTO cached_path (id, hash, package, file_hash, created_at) VALUES (uuidv7(), 'lgd', 'd', 'sha256:d', now());" >/dev/null
      fi
      : > $D/a.out
      : > $D/b.out
    }

    recount() {
      n=$(printf '%s\n' "SET search_path = lockguard, public;" "$RECOUNT;" | pg | grep '^UPDATE' | awk '{print $2}')
      if grep -q ERROR $D/a.out $D/b.out; then fail "$1: a session reported an error"; fi
      echo "lockguard $1: recount wrote $n"
    }

    rm -rf $D
    mkdir -p $D
    q "DROP SCHEMA IF EXISTS lockguard CASCADE; CREATE SCHEMA lockguard;
       CREATE TABLE lockguard.derivation_build (LIKE public.derivation_build INCLUDING DEFAULTS INCLUDING INDEXES);
       CREATE TABLE lockguard.derivation_dependency (LIKE public.derivation_dependency INCLUDING DEFAULTS INCLUDING INDEXES);
       CREATE TABLE lockguard.derivation_output (LIKE public.derivation_output INCLUDING DEFAULTS INCLUDING INDEXES);
       CREATE TABLE lockguard.cached_path (LIKE public.cached_path INCLUDING DEFAULTS INCLUDING INDEXES);" >/dev/null
    mkfifo $D/a.in
    mkfifo $D/b.in
    reset absent
    pg < $D/a.in >> $D/a.out 2>&1 &
    APID=$!
    pg < $D/b.in >> $D/b.out 2>&1 &
    BPID=$!
    exec 3> $D/a.in
    exec 4> $D/b.in
    send_a "SET application_name = 'lockguard_a'; SET search_path = lockguard, public;"
    send_b "SET application_name = 'lockguard_b'; SET search_path = lockguard, public;"

    send_b "BEGIN;"
    send_b "$(bind "$LOCK_SHARED_BUILDS" "'{$DEP}'")"
    send_b "INSERT INTO cached_path (id, hash, package, file_hash, created_at) VALUES (uuidv7(), 'lgd', 'd', 'sha256:d', now());"
    idle_tx b "flip-first: the flip never held D"
    send_a "BEGIN;"
    send_a "INSERT INTO derivation_dependency (derivation, dependency, kind) VALUES ('$P', '$DEP', 1);"
    send_a "$(bind "$LOCK_SEED" "'{$P}'")"
    blocked a "flip-first: the seed of P did not wait on D's key"
    send_b "$(bind "$SEED" "'{$DEP}'" "'{t}'")"
    send_b "$(bind "$RIPPLE" "'{$DEP}'" "true")"
    send_b "COMMIT;"
    idle b "flip-first: the flip never committed"
    idle_tx a "flip-first: the seed never resumed"
    send_a "$(bind "$SEED" "'{$P}'" "'{f}'")"
    send_a "COMMIT;"
    idle a "flip-first: the seed never committed"
    recount flip-first

    reset absent
    send_b "BEGIN;"
    send_b "$(bind "$LOCK_SHARED_BUILDS" "'{$DEP}'")"
    send_b "INSERT INTO cached_path (id, hash, package, file_hash, created_at) VALUES (uuidv7(), 'lgd', 'd', 'sha256:d', now());"
    idle_tx b "unguarded: the flip never held D"
    send_a "BEGIN;"
    send_a "INSERT INTO derivation_dependency (derivation, dependency, kind) VALUES ('$P', '$DEP', 1);"
    send_a "$(bind "$LOCK_SHARED_BUILDS" "'{$P}'")"
    send_a "$(bind "$SEED" "'{$P}'" "'{f}'")"
    idle_tx a "unguarded: the seed blocked without the shared keys"
    send_b "$(bind "$SEED" "'{$DEP}'" "'{t}'")"
    send_b "$(bind "$RIPPLE" "'{$DEP}'" "true")"
    send_b "COMMIT;"
    idle b "unguarded: the flip never committed"
    send_a "COMMIT;"
    idle a "unguarded: the seed never committed"
    recount unguarded

    reset absent
    send_a "BEGIN;"
    send_a "INSERT INTO derivation_dependency (derivation, dependency, kind) VALUES ('$P', '$DEP', 1);"
    send_a "$(bind "$LOCK_SEED" "'{$P}'")"
    send_a "$(bind "$SEED" "'{$P}'" "'{f}'")"
    idle_tx a "seed-first: the seed never held D's key"
    send_b "BEGIN;"
    send_b "$(bind "$LOCK_SHARED_BUILDS" "'{$DEP}'")"
    blocked b "seed-first: the flip did not wait on the seed"
    send_a "COMMIT;"
    idle a "seed-first: the seed never committed"
    idle_tx b "seed-first: the flip never resumed"
    send_b "INSERT INTO cached_path (id, hash, package, file_hash, created_at) VALUES (uuidv7(), 'lgd', 'd', 'sha256:d', now());"
    send_b "$(bind "$SEED" "'{$DEP}'" "'{t}'")"
    send_b "$(bind "$RIPPLE" "'{$DEP}'" "true")"
    send_b "COMMIT;"
    idle b "seed-first: the flip never committed"
    grep -q "^$P$" $D/b.out || fail "seed-first: the ripple did not see P's edge"
    recount seed-first

    reset present
    q "INSERT INTO derivation_dependency (derivation, dependency, kind) VALUES ('$P', '$DEP', 1);" >/dev/null
    send_a "BEGIN;"
    send_a "INSERT INTO derivation_dependency (derivation, dependency, kind) VALUES ('$Q', '$DEP', 1);"
    send_a "$(bind "$LOCK_SEED" "'{$Q}'")"
    send_a "$(bind "$SEED" "'{$Q}'" "'{f}'")"
    idle_tx a "incomplete: the seed of Q never held D's key"
    send_b "BEGIN;"
    send_b "$(bind "$LOCK_PATHS" "'{lgd}'")"
    blocked b "incomplete: the retire did not wait on the seed"
    send_a "COMMIT;"
    idle a "incomplete: the seed never committed"
    idle_tx b "incomplete: the retire never resumed"
    send_b "$(bind "$CLOSURE_COMPLETE_AMONG" "'{$DEP}'")"
    send_b "DELETE FROM cached_path WHERE hash = 'lgd';"
    send_b "$(bind "$RIPPLE" "'{$DEP}'" "false")"
    send_b "COMMIT;"
    idle b "incomplete: the retire never committed"
    grep -q "^$Q$" $D/b.out || fail "incomplete: the ripple did not see Q's edge"
    recount incomplete

    reset present
    q "UPDATE derivation_build SET missing_runtime_deps = 1 WHERE derivation = '$DEP';
       INSERT INTO derivation_dependency (derivation, dependency, kind) VALUES ('$DEP', '$Q', 1);" >/dev/null
    send_a "BEGIN;"
    send_a "$(bind "$RIPPLE" "'{$Q}'" "true")"
    idle_tx a "retire-reads: the flip never held D"
    send_b "BEGIN;"
    send_b "$(bind "$LOCK_PATHS" "'{lgd}'")"
    blocked b "retire-reads: the retire did not wait on the flip"
    send_a "COMMIT;"
    idle a "retire-reads: the flip never committed"
    idle_tx b "retire-reads: the retire never resumed"
    send_b "$(bind "$CLOSURE_COMPLETE_AMONG" "'{$DEP}'")"
    send_b "ROLLBACK;"
    idle b "retire-reads: the retire never finished"
    grep -q "^$DEP$" $D/b.out || fail "retire-reads: the retire read D as it was before the flip it waited for"
    recount retire-reads

    reset present
    send_a "BEGIN;"
    send_a "INSERT INTO derivation_dependency (derivation, dependency, kind) VALUES ('$P', '$DEP', 1);"
    send_a "$(bind "$LOCK_SEED" "'{$P}'")"
    idle_tx a "shared: the first seed never held D's key"
    send_b "BEGIN;"
    send_b "INSERT INTO derivation_dependency (derivation, dependency, kind) VALUES ('$Q', '$DEP', 1);"
    send_b "$(bind "$LOCK_SEED" "'{$Q}'")"
    idle_tx b "shared: the second seed waited on the first"
    send_a "$(bind "$SEED" "'{$P}'" "'{f}'")"
    send_b "$(bind "$SEED" "'{$Q}'" "'{f}'")"
    send_a "COMMIT;"
    send_b "COMMIT;"
    idle a "shared: the first seed never committed"
    idle b "shared: the second seed never committed"
    recount shared

    q "DROP SCHEMA lockguard CASCADE;" >/dev/null
    """

    start_all()

    banner("Phase 1: bring services up")
    server.wait_for_unit("gradient-server.service")
    server.sleep(5)

    wait_workers_ready()
    banner("Every worker authenticated via state-managed registration")

    banner("Phase 2: prepare test repository")
    server.succeed(f"{GIT} config --global --add safe.directory '*'")
    server.succeed(f"{GIT} config --global init.defaultBranch main")
    server.succeed(f"{GIT} config --global user.email 'nixos@localhost'")
    server.succeed(f"{GIT} config --global user.name 'NixOS test'")

    server.succeed(f"{GIT} init /var/lib/git/test")
    server.succeed("cp /var/lib/git/{,test/}flake.nix")
    server.succeed("cp /var/lib/git/{,test/}flake.lock")

    server.succeed("sed -i 's#\\[nixpkgs\\]#${self.inputs.nixpkgs}#g' /var/lib/git/test/flake.nix")
    server.succeed("sed -i 's#\\[nixpkgs\\]#${self.inputs.nixpkgs}#g' /var/lib/git/test/flake.lock")
    server.succeed("sed -i 's#\\[hash\\]#${self.inputs.nixpkgs.narHash}#g' /var/lib/git/test/flake.lock")

    server.succeed(f"{GIT} -C /var/lib/git/test add flake.nix flake.lock")
    server.succeed(f"{GIT} -C /var/lib/git/test commit -m 'Initial commit'")
    server.succeed("chown git:git -R /var/lib/git/test")

    server.succeed(f"{GIT} clone git://localhost/test test")
    print(server.succeed(f"{GIT} ls-remote git://server/test"))

    banner("Phase 3: authenticate and select task")
    login_body = '{"loginname": "admin", "password": "admin_password"}'
    token = server.succeed(
        f"{CURL} -X POST -H 'Content-Type: application/json' "
        f"-d '{login_body}' {API}/auth/basic/login | {JQ} -rj '.message'"
    ).strip()
    print(f"Got token: {token[:20]}…")

    server.succeed(f"{CLI} config Server http://gradient.local")
    server.succeed(f"{CLI} config AuthToken {token}")
    server.succeed(f"{CLI} project select project")
    server.succeed(f"{CLI} task select task")

    server.sleep(10)
    _, output = server.execute(f"{CLI} task show")
    print(output)

    banner("Phase 4: wait for repository detection")
    detected = False
    for attempt in range(1, 7):
        server.sleep(15)
        j = assert_no_server_panic(since_seconds=attempt * 15 + 15)
        if any(needle in j for needle in (
            "update needed", "Force evaluation", "trigger created evaluation", "Queued"
        )):
            detected = True
            banner(f"Repository update detected on attempt {attempt}")
            break
    if not detected:
        raise Exception(f"Server did not detect repository change after 90 s:\n{j[-2000:]}")

    banner("Phase 5: wait for evaluation to complete (up to 900 s)")
    eval_id = ""
    completed = False
    for attempt in range(1, 91):
        server.sleep(10)
        assert_no_server_panic(since_seconds=15)

        print("  DEBUG get_task: " + server.succeed(
            f'{CURL} -s -w " [HTTP %{{http_code}}]" -H "Authorization: Bearer {token}" {API}/tasks/project/task'
        ))
        eval_id = server.succeed(
            f'{CURL} -sf -H "Authorization: Bearer {token}" '
            f'{API}/tasks/project/task | {JQ} -rj ".message.last_evaluation // empty"'
        ).strip()
        if not eval_id:
            if attempt % 3 == 0:
                print(f"  [{attempt:>2}/90] still waiting for evaluation to start…")
            continue

        eval_status = server.succeed(
            f'{CURL} -sf -H "Authorization: Bearer {token}" '
            f'{API}/evals/{eval_id} | {JQ} -rj ".message.status"'
        ).strip()

        if eval_status == "Completed":
            completed = True
            banner(f"Evaluation completed on attempt {attempt}")
            break

        if eval_status == "Failed":
            j  = server.succeed("journalctl -u gradient-server --no-pager --since='-300s' -n 200")
            fleet_logs = "".join(
                f"\n{node.name} {unit}:\n"
                + node.succeed(f"journalctl -u {unit} --no-pager --since='-300s' -n 200")[-2000:]
                for node, unit in fleet_units()
            )
            raise Exception(f"Evaluation failed:\nServer:\n{j[-2000:]}\nFleet:{fleet_logs}")

        if attempt % 3 == 0:
            eval_detail = server.succeed(
                f'{CURL} -sf -H "Authorization: Bearer {token}" '
                f'{API}/evals/{eval_id} | '
                f'{JQ} -c ".message | {{status, entry_points: (.entry_points | length)}}"'
            ).strip()
            builds_summary = server.succeed(
                f'{CURL} -sf -H "Authorization: Bearer {token}" '
                f'{API}/evals/{eval_id}/builds | '
                f'{JQ} -c ".message | {{total, by_status: ([.builds[].status] | group_by(.) | map({{key: .[0], value: length}}) | from_entries)}}"'
            ).strip()
            fleet = server.succeed(
                f'{CURL} -sf -H "Authorization: Bearer {token}" '
                f'{API}/board/health | '
                f'{JQ} -c ".message | {{workers: .workers_connected, sessions: .proto_sessions, '
                f'pending: .jobs_pending, active: .jobs_active}}"'
            ).strip()
            print(
                f"  [{attempt:>2}/90] eval={eval_detail} builds={builds_summary} "
                f"fleet={fleet}"
            )

    if not completed:
        eval_state = sql(
            f"SELECT status::text || ' since ' || updated_at::text"
            f" FROM evaluation WHERE id = '{eval_id}';"
        )
        shared_builds = sql(
            f"SELECT db.status::text || ' fetchable=' || db.fetchable::int::text"
            f"  || ' wanted=' || db.wanted::int::text"
            f"  || ' blocking_deps=' || db.blocking_deps || ' count=' || count(*)::text"
            f" FROM derivation_build db"
            f" JOIN build_job bj ON bj.derivation_build = db.id"
            f" WHERE bj.evaluation = '{eval_id}'"
            f" GROUP BY db.status, db.fetchable, db.wanted, db.blocking_deps ORDER BY 1;"
        )
        gates = blocking_shared_builds(eval_id)
        noise = (
            "gradient_web: (request started|response generated"
            "|sending chunk|stream closed)"
            "|gradient_sources::git::(update_check|commit_info|pktline)"
            "|gradient_cache::cacher: (Cache cleanup|Evaluation GC|Derivation GC)"
            "|handler::dispatch: WorkerMetrics"
        )
        j = server.succeed(
            f"journalctl -u gradient-server --no-pager --since='-900s' -n 8000"
            f" | grep -vE '{noise}' | tail -n 60"
        )
        fleet = server.succeed(
            "journalctl -u gradient-server --no-pager --since='-900s' -n 8000"
            " | grep -E 'handshake complete|duplicate connection|worker silent"
            "|unregister|job accepted|job rejected|abandoned|not streaming'"
            " | tail -n 40"
        )
        workers = "".join(
            f"\n{node.name} {unit}:\n"
            + node.succeed(
                f"journalctl -u {unit} --no-pager --since='-300s' -n 400"
                " | tail -n 40"
            )
            for node, unit in fleet_units()
        )
        raise Exception(
            f"Evaluation did not complete after 900 s. Evaluation is {eval_state}.\n"
            f"shared builds of this evaluation:\n{shared_builds}\n"
            f"what the evaluation still waits on, by gate:\n{gates}\n"
            f"fleet events over the window:\n{fleet}\n"
            f"worker journals:{workers}\n"
            f"server log, polling and GC noise removed:\n{j}"
        )

    banner("Phase 5b: worker eval-cache is committed (non-empty)")
    eval_cache_dir = "/var/lib/gradient-worker/eval-cache/eval-cache-v6"
    listings = [
        node.succeed(f"find {eval_cache_dir} -name '*.sqlite' -printf '%s %p\\n' 2>/dev/null || true").strip()
        for node in WORKER_NODES
    ]
    lines = [line for listing in listings for line in listing.splitlines() if line]
    sizes = "\n".join(sorted(lines, key=lambda line: -int(line.split()[0])))
    print(sizes or "(no .sqlite files)")
    biggest = int(sizes.splitlines()[0].split()[0]) if sizes else 0
    assert biggest > 4096, (
        f"eval-cache never committed: largest .sqlite is {biggest} bytes "
        f"(4096 = empty SQLite header).\n{sizes}"
    )

    banner("Phase 5c: a concurrent evaluation of the same commit merged cleanly")
    eval2_id = ""
    for attempt in range(1, 61):
        eval2_id = server.succeed(
            f'{CURL} -sf -H "Authorization: Bearer {token}" '
            f'{API}/tasks/project/task2 | {JQ} -rj ".message.last_evaluation // empty"'
        ).strip()
        if eval2_id:
            status2 = server.succeed(
                f'{CURL} -sf -H "Authorization: Bearer {token}" '
                f'{API}/evals/{eval2_id} | {JQ} -rj ".message.status"'
            ).strip()
            if status2 == "Completed":
                break
            if status2 == "Failed":
                j = server.succeed("journalctl -u gradient-server --no-pager --since='-600s' -n 300")
                raise Exception(f"second evaluation failed:\n{j[-3000:]}")
        server.sleep(10)
    else:
        raise Exception("second evaluation did not complete after 600 s")
    assert eval2_id != eval_id, "task2 must have its own evaluation"

    def reach(name, evaluation):
        return (
            f"{name}(derivation) AS ("
            f"  SELECT derivation FROM build_job WHERE evaluation = '{evaluation}'"
            f"  UNION SELECT e.dependency FROM derivation_dependency e "
            f"  JOIN {name} c ON e.derivation = c.derivation)"
        )

    reach1 = reach("reach1", eval_id)
    reach2 = reach("reach2", eval2_id)
    builds1 = int(sql(f"SELECT count(*) FROM build_job WHERE evaluation = '{eval_id}';"))
    builds2 = int(sql(f"SELECT count(*) FROM build_job WHERE evaluation = '{eval2_id}';"))
    closure1 = int(sql(f"WITH RECURSIVE {reach1} SELECT count(*) FROM reach1;"))
    closure2 = int(sql(f"WITH RECURSIVE {reach2} SELECT count(*) FROM reach2;"))
    print(f"build jobs: eval1={builds1} eval2={builds2}; closures: eval1={closure1} eval2={closure2}")
    assert builds1 > 0 and builds2 > 0, "both evaluations record builds"
    assert closure1 > 0 and closure1 == closure2, "both evaluations reach the same number of derivations"
    differ = int(sql(
        f"WITH RECURSIVE {reach1}, {reach2} "
        f"SELECT (SELECT count(*) FROM (SELECT derivation FROM reach1 "
        f"        EXCEPT SELECT derivation FROM reach2) a) "
        f"     + (SELECT count(*) FROM (SELECT derivation FROM reach2 "
        f"        EXCEPT SELECT derivation FROM reach1) b);"
    ))
    assert differ == 0, f"the two evaluations' closures differ on {differ} derivations"
    unnamed = int(sql(
        f"WITH RECURSIVE {reach1} "
        f"SELECT count(*) FROM reach1 r WHERE NOT EXISTS ("
        f"  SELECT 1 FROM build_job bj WHERE bj.derivation = r.derivation "
        f"  AND bj.evaluation IN ('{eval_id}', '{eval2_id}'));"
    ))
    assert unnamed == 0, f"{unnamed} derivations of the closure were named by neither walk"
    duplicates = int(sql("SELECT count(*) - count(DISTINCT hash) FROM derivation;"))
    assert duplicates == 0, f"{duplicates} duplicate derivation rows"
    unwalked = int(sql(
        f"WITH RECURSIVE {reach1} "
        f"SELECT count(*) FROM reach1 r JOIN derivation d ON d.id = r.derivation WHERE NOT d.walked;"
    ))
    assert unwalked == 0, f"{unwalked} derivations of the closure are stubs"
    edges = int(sql("SELECT count(*) FROM derivation_dependency;"))
    assert edges > 0, "the graph recorded no dependency edge at all"
    unwalked_deps = int(sql(
        "SELECT count(*) FROM derivation_dependency e JOIN derivation d ON d.id = e.dependency "
        "WHERE NOT d.walked;"
    ))
    assert unwalked_deps == 0, f"{unwalked_deps} of {edges} dependency edges point at a stub"
    incomplete = sql(
        "SELECT d.name || ' unwalked_inputs=' || d.unwalked_inputs::text"
        " FROM derivation d WHERE d.walked AND d.unwalked_inputs <> 0"
        " ORDER BY d.unwalked_inputs, d.name LIMIT 20;"
    )
    assert not incomplete, (
        f"walked derivations still count an unwalked input after a complete walk:\n{incomplete}"
    )
    assert_no_server_error(server.succeed("journalctl -u gradient-server --no-pager"))

    banner("Phase 5b: graph walk shape, plans and closure counters")

    have_reverse_index = int(sql(
        "SELECT count(*) FROM pg_indexes "
        "WHERE indexname = 'idx-derivation_dependency-reverse-pair';"
    ))
    assert have_reverse_index == 1, "the reverse walk lost its covering index"
    have_runtime_index = int(sql(
        "SELECT count(*) FROM pg_indexes "
        "WHERE indexname = 'idx-derivation_dependency-runtime';"
    ))
    assert have_runtime_index == 1, "the runtime walk lost its covering index"
    leftover_keys = int(sql(
        "SELECT count(*) FROM information_schema.columns "
        "WHERE column_name = 'id' AND table_name = 'derivation_dependency';"
    ))
    assert leftover_keys == 0, f"{leftover_keys} junction tables kept a surrogate key"
    retired_index = int(sql(
        "SELECT count(*) FROM information_schema.tables "
        "WHERE table_name = 'cached_path_reference';"
    ))
    assert retired_index == 0, "the path-level reference index outlived its migration"

    for direction, fenced_step, plain_step in [
        ("dependencies",
         "SELECT e.dependency AS next FROM derivation_dependency e WHERE e.derivation = c.derivation",
         "SELECT e.dependency FROM derivation_dependency e JOIN plain c ON e.derivation = c.derivation"),
        ("parents",
         "SELECT e.derivation AS next FROM derivation_dependency e WHERE e.dependency = c.derivation",
         "SELECT e.derivation FROM derivation_dependency e JOIN plain c ON e.dependency = c.derivation"),
    ]:
        seed = f"SELECT bj.derivation FROM build_job bj WHERE bj.evaluation = '{eval_id}'"
        disagree = int(sql(
            f"WITH RECURSIVE fenced(derivation) AS ({seed} UNION "
            f"  SELECT s.next FROM fenced c, LATERAL ({fenced_step} OFFSET 0) s), "
            f"plain(derivation) AS ({seed} UNION {plain_step}) "
            f"SELECT (SELECT count(*) FROM (SELECT derivation FROM fenced "
            f"        EXCEPT SELECT derivation FROM plain) a) "
            f"     + (SELECT count(*) FROM (SELECT derivation FROM plain "
            f"        EXCEPT SELECT derivation FROM fenced) b);"
        ))
        assert disagree == 0, f"the fenced {direction} walk disagrees on {disagree} nodes"

    gone = int(sql(
        "SELECT count(*) FROM information_schema.tables "
        "WHERE table_name = 'derivation_closure';"
    ))
    assert gone == 0, "derivation_closure is still there"

    sql(
        f"WITH d AS ("
        f"  INSERT INTO derivation (id, hash, name, architecture, created_at) "
        f"  VALUES (uuidv7(), 'zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz', 'unreportable', "
        f"          'x86_64-linux', now() AT TIME ZONE 'UTC') RETURNING id) "
        f"INSERT INTO entry_point (id, task, evaluation, derivation, eval, created_at) "
        f"SELECT uuidv7(), ep.task, ep.evaluation, d.id, 'zz.unreportable', "
        f"       now() AT TIME ZONE 'UTC' "
        f"FROM d, entry_point ep WHERE ep.evaluation = '{eval_id}' LIMIT 1;"
    )
    planted = int(sql(
        f"SELECT count(*) FROM entry_point WHERE evaluation = '{eval_id}' "
        f"AND eval = 'zz.unreportable';"
    ))
    assert planted == 1, "the unreportable entry point was not planted"

    reportable = (
        f"ep.evaluation = '{eval_id}' AND EXISTS ("
        f"  SELECT 1 FROM build_job bj WHERE bj.evaluation = '{eval_id}'"
        f"  AND bj.derivation = ep.derivation)"
    )
    version_before = int(sql(
        f"SELECT graph_version FROM evaluation WHERE id = '{eval_id}';"
    ))
    page = json.loads(api_get(
        token, f"tasks/project/task/entry-points?evaluation_id={eval_id}&limit=500"
    ))["message"]
    assert page["total"] == len(page["entry_points"]) > 0, page
    unstamped = int(sql(
        f"SELECT count(*) FROM entry_point ep WHERE {reportable} "
        f"AND (ep.dep_counts_version IS NULL "
        f"     OR ep.dep_counts_version < {version_before});"
    ))
    assert unstamped == 0, f"{unstamped} entry points were not stamped by the read"

    drift = int(sql(
        f"WITH RECURSIVE stored AS ("
        f"  SELECT ep.id, coalesce(sum(c.count), 0) AS total FROM entry_point ep "
        f"  LEFT JOIN entry_point_dep_count c ON c.entry_point = ep.id "
        f"  WHERE {reportable} GROUP BY ep.id), "
        f"closure(ep, drv) AS ("
        f"  SELECT ep.id, ep.derivation FROM entry_point ep WHERE {reportable} "
        f"  UNION SELECT c.ep, s.next FROM closure c, LATERAL ("
        f"    SELECT dd.dependency AS next FROM derivation_dependency dd "
        f"    JOIN build_job bj ON bj.derivation = dd.dependency AND bj.evaluation = '{eval_id}' "
        f"    WHERE dd.derivation = c.drv OFFSET 0) s), "
        f"live AS ("
        f"  SELECT ep.id, count(*) AS total FROM entry_point ep "
        f"  JOIN closure c ON c.ep = ep.id AND c.drv <> ep.derivation "
        f"  JOIN build_job bj ON bj.derivation = c.drv AND bj.evaluation = '{eval_id}' "
        f"  WHERE {reportable} GROUP BY ep.id) "
        f"SELECT count(*) FROM live l JOIN stored s ON s.id = l.id WHERE l.total <> s.total;"
    ))
    assert drift == 0, f"{drift} entry points disagree with the live walk"
    by_id = {ep["id"]: ep for ep in page["entry_points"]}
    stored_totals = sql(
        f"SELECT ep.id || ':' || coalesce(sum(c.count), 0) FROM entry_point ep "
        f"LEFT JOIN entry_point_dep_count c ON c.entry_point = ep.id "
        f"WHERE {reportable} GROUP BY ep.id;"
    ).split()
    for row in stored_totals:
        ep_id, total = row.split(":")
        assert by_id[ep_id]["deps_total"] == int(total), f"{ep_id}: api {by_id[ep_id]['deps_total']} stored {total}"

    sql(
        f"DELETE FROM entry_point WHERE evaluation = '{eval_id}' "
        f"AND eval = 'zz.unreportable';"
    )
    sql("DELETE FROM derivation WHERE hash = 'zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz';")
    left = int(sql(
        f"SELECT count(*) FROM entry_point WHERE evaluation = '{eval_id}' "
        f"AND eval = 'zz.unreportable';"
    ))
    assert left == 0, "the unreportable entry point outlived its assertions"

    banner("Phase 6: extract hello's derivation path from /evals/{id}/builds")
    store_path_drv = server.succeed(
        f'{CURL} -sf -H "Authorization: Bearer {token}" '
        f'{API}/evals/{eval_id}/builds | '
        f'{JQ} -r \'.message.builds[] | select(.name | test("hello[^/]*\\\\.drv$")) | .name\' | head -n1'
    ).strip()
    assert store_path_drv, f"could not find hello's .drv in eval {eval_id}'s builds"
    if not store_path_drv.startswith("/nix/store/"):
        store_path_drv = f"/nix/store/{store_path_drv}"

    store_path = builder.succeed(
        f"{NIX} path-info {store_path_drv}^out --extra-experimental-features nix-command"
    ).strip()
    store_hash = store_path.split("-")[0].replace("/nix/store/", "")
    print(f"Built derivation: {store_path_drv}")
    print(f"Output path:      {store_path}")

    drv_hash = store_path_drv.split("/")[-1].split("-")[0]
    declared = int(builder.succeed(
        f"{NIX} derivation show {store_path_drv} --extra-experimental-features nix-command "
        f"| {JQ} '[.derivations[] | (.inputDrvs // .inputs.drvs // {{}}) | length] | add'"
    ).strip())
    recorded = int(sql(
        f"SELECT count(*) FROM derivation_dependency e JOIN derivation d ON d.id = e.derivation "
        f"WHERE d.hash = '{drv_hash}' AND e.kind IN (0, 2);"
    ))
    assert declared > 0, f"{store_path_drv} declares no input drv; the check would pass on nothing"
    assert declared == recorded, f"hello declares {declared} input drvs, the graph records {recorded}"

    banner("Phase 7: cache serves nix-cache-info and the narinfo")
    print(client.succeed(f"{CURL} {CACHE}/nix-cache-info -i --fail"))

    for sig_attempt in range(1, 25):
        rc, _ignored = client.execute(f"{CURL} -sf {CACHE}/{store_hash}.narinfo -o /dev/null")
        if rc == 0:
            banner(f"narinfo signed and served on poll {sig_attempt}")
            break
        client.sleep(5)
    print(client.succeed(f"{CURL} {CACHE}/{store_hash}.narinfo -i --fail"))

    banner("Phase 8: client realizes hello from gradient cache")
    client.succeed(f"nix-store --delete {store_path} || true")
    client.fail(f"ls {store_path}")
    print(client.succeed(f"nix-store -vvv --realize {store_path}"))
    print(client.succeed(f"ls {store_path}"))

    banner("Phase 9: gradient cache upload compresses + signs (#509)")
    server.succeed("echo gradient-upload-regression-509 > /tmp/upload-probe.txt")
    upload_path = server.succeed("nix-store --add /tmp/upload-probe.txt").strip()
    upload_hash = upload_path.split("-")[0].replace("/nix/store/", "")
    print(f"Uploading probe path: {upload_path}")
    print(server.succeed(f"{CLI} cache upload main {upload_path}"))

    client.wait_until_succeeds(
        f"{CURL} -sf {CACHE}/{upload_hash}.narinfo -o /dev/null", timeout=60
    )
    print(client.succeed(f"{CURL} {CACHE}/{upload_hash}.narinfo -i --fail"))

    client.fail(f"ls {upload_path}")
    print(client.succeed(f"nix-store -vvv --realize {upload_path}"))
    print(client.succeed(f"ls {upload_path}"))

    banner("Phase 10: debuginfo index and 404 on unknown keys (#563)")
    build_id = "7dbeaca53fbc9a489b633871093c37dae3857a37"
    member = f"lib/debug/.build-id/{build_id[:2]}/{build_id[2:]}.debug"
    server.succeed(f"mkdir -p /tmp/probe-debug/lib/debug/.build-id/{build_id[:2]}")
    server.succeed(f"echo gradient-debuginfo-563 > /tmp/probe-debug/{member}")
    debug_path = server.succeed("nix-store --add /tmp/probe-debug").strip()
    debug_hash = debug_path.split("-")[0].replace("/nix/store/", "")
    print(f"Uploading debug output: {debug_path}")
    print(server.succeed(f"{CLI} cache upload main {debug_path}"))

    client.wait_until_succeeds(
        f"{CURL} -sf {CACHE}/{debug_hash}.narinfo -o /dev/null", timeout=60
    )

    client.wait_until_succeeds(
        f"{CURL} -sf {CACHE}/debuginfo/{build_id} -o /dev/null", timeout=60
    )
    redirect = client.succeed(f"{CURL} -sf {CACHE}/debuginfo/{build_id}")
    print(redirect)
    parsed = json.loads(redirect)
    assert parsed["member"] == member, redirect
    assert parsed["archive"].startswith("../nar/"), redirect
    assert parsed["archive"].endswith(".nar.zst"), redirect

    assert client.succeed(f"{CURL} -sf {CACHE}/debuginfo/{build_id}.debug") == redirect

    archive = parsed["archive"].replace("../", "", 1)
    client.succeed(f"{CURL} -sf {CACHE}/{archive} -o /dev/null")

    def status(url):
        return client.succeed(f"{CURL} -s -o /dev/null -w '%{{http_code}}' {url}").strip()

    assert status(f"{CACHE}/debuginfo/{'0' * 40}") == "404"
    assert status(f"{CACHE}/debuginfo/{'0' * 40}.debug") == "404"
    assert status(f"{CACHE}/debuginfo/not-a-build-id") == "404"
    assert status(f"http://server/cache/debuginfo/{build_id}.debug") == "404"

    banner("Phase 10b: the completed build job has worker phase spans")
    job_id, build_status = sql(
        f"SELECT dj.id, db.status FROM dispatched_job dj "
        f"JOIN build_job bj ON dj.job_id = 'build:' || bj.derivation_build::text "
        f"JOIN derivation_build db ON db.id = bj.derivation_build "
        f"WHERE bj.evaluation = '{eval_id}' AND dj.kind = 1 AND dj.finished_at IS NOT NULL "
        f"ORDER BY dj.dispatched_at DESC LIMIT 1;"
    ).partition("|")[::2]
    assert job_id, "no finished build job was recorded for the evaluation"

    job = json.loads(api_get(token, f"board/jobs/{job_id}"))["message"]
    phases = {p["phase"] for p in job["phases"]}
    print(f"job {job_id} (status {build_status}) phases: {sorted(phases)}")
    assert ("build" in phases) == (build_status == "3"), f"status {build_status} with {sorted(phases)}"
    assert "nar_push" in phases, f"no nar push span in {sorted(phases)}"
    assert all(p["end_ms"] >= p["start_ms"] for p in job["phases"]), job["phases"]
    assert job["outcome"] == "completed", job["outcome"]
    assert job["finished_at"] is not None
    nested = [p for p in job["phases"] if p["parent_seq"] is not None]
    assert nested, "the timeline recorded no nesting at all"

    eval_ms = sql(
        f"SELECT fetch_ms + eval_flake_ms + eval_drv_ms FROM evaluation_metric "
        f"WHERE evaluation = '{eval_id}' LIMIT 1;"
    )
    assert eval_ms and int(eval_ms) > 0, f"eval phase columns not derived from the timeline: {eval_ms!r}"

    banner("Phase 10c: missing_runtime_deps moves on retire and re-upload")

    retired_columns = int(sql(
        "SELECT count(*) FROM information_schema.columns "
        "WHERE table_name = 'cached_path' "
        "  AND column_name IN ('closure_complete', 'missing_references');"
    ))
    assert retired_columns == 0, "cached_path still carries a retired complete-closure column"

    sql("CREATE EXTENSION IF NOT EXISTS pg_stat_statements;")
    COUNTER_WRITES = "s.query ILIKE '%update derivation_build%missing_runtime_deps%'"
    counter_rows_before = int(sql(
        f"SELECT coalesce(sum(s.rows), 0) FROM pg_stat_statements s WHERE {COUNTER_WRITES};"
    ))

    def shared_build_drift():
        return int(sql(
            "SELECT count(*) FROM derivation_build db WHERE db.blocking_deps <> ("
            "  SELECT count(*) FROM derivation_dependency e "
            "  LEFT JOIN derivation_build dep ON dep.derivation = e.dependency "
            "  WHERE e.derivation = db.derivation AND e.kind IN (0, 2) "
            "    AND (dep.derivation IS NULL OR NOT dep.fetchable)) "
            "OR db.fetchable <> (db.status IN (3, 7) AND db.missing_runtime_deps = 0 "
            "AND EXISTS ("
            "  SELECT 1 FROM derivation_output o2 WHERE o2.derivation = db.derivation) "
            "AND NOT EXISTS ("
            "  SELECT 1 FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash "
            "  WHERE o.derivation = db.derivation AND cp.file_hash IS NULL));"
        ))

    def runtime_drift():
        return int(sql(
            "WITH RECURSIVE incomplete(derivation) AS ("
            "  SELECT db.derivation FROM derivation_build db WHERE NOT ("
            "    EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = db.derivation) "
            "    AND NOT EXISTS (SELECT 1 FROM derivation_output o "
            "                    LEFT JOIN cached_path cp ON cp.hash = o.hash "
            "                    WHERE o.derivation = db.derivation AND cp.file_hash IS NULL)) "
            "  UNION "
            "  SELECT e.derivation FROM derivation_dependency e "
            "  JOIN incomplete u ON u.derivation = e.dependency WHERE e.kind IN (1, 2)) "
            "SELECT count(*) FROM derivation_build db WHERE db.missing_runtime_deps <> ("
            "  SELECT count(*) FROM derivation_dependency e "
            "  JOIN incomplete u ON u.derivation = e.dependency "
            "  WHERE e.derivation = db.derivation AND e.kind IN (1, 2));"
        ))

    def shared_build_complete(drv):
        return sql(
            f"SELECT db.missing_runtime_deps::text || ' ' || db.fetchable::int::text "
            f"FROM derivation_build db JOIN derivation d ON d.id = db.derivation "
            f"WHERE d.hash = '{drv}';"
        ).split()

    def wanted_drift():
        def is_open(a):
            return f"NOT {a}.fetchable AND {a}.status NOT IN (4, 6, 9)"
        def is_builder(a):
            return f"w.walked AND {a}.probed AND NOT {a}.cache_available AND {a}.status IN (0, 1, 2, 8)"
        return int(sql(
            "WITH RECURSIVE wanted(derivation, builder) AS ("
            f"  SELECT db.derivation, ({is_builder('db')}) FROM entry_point ep "
            "  JOIN derivation_build db ON db.derivation = ep.derivation "
            f"  JOIN derivation w ON w.id = db.derivation WHERE {is_open('db')} "
            "  UNION "
            f"  SELECT e.dependency, ({is_builder('dep')}) FROM wanted c "
            "  JOIN derivation_dependency e ON e.derivation = c.derivation "
            "  JOIN derivation_build dep ON dep.derivation = e.dependency "
            "  JOIN derivation w ON w.id = dep.derivation "
            f"  WHERE (c.builder OR e.kind IN (1, 2)) AND {is_open('dep')}) "
            f"SELECT count(*) FROM derivation_build db WHERE {is_open('db')} "
            "AND db.wanted <> (db.derivation IN (SELECT derivation FROM wanted));"
        ))

    def unbacked():
        return int(sql(
            "SELECT count(DISTINCT o.hash) FROM derivation_output o "
            "JOIN derivation_build db ON db.derivation = o.derivation "
            "WHERE db.status IN (3, 7) AND o.external_url IS NULL "
            "  AND NOT EXISTS (SELECT 1 FROM cached_path cp "
            "                  WHERE cp.hash = o.hash AND cp.file_hash IS NOT NULL);"
        ))

    def poll(query, want, what, timeout=180):
        for _ in range(timeout):
            if sql(query) == want:
                return
            server.sleep(1)
        raise Exception(f"{what} (still {sql(query)!r}, want {want!r})")

    assert runtime_drift() == 0, "complete-closure counter disagrees with its recount before the retire"
    assert shared_build_drift() == 0, "shared build counters disagree with their recount before the retire"
    assert unbacked() == 0, "a producer this build settled has an output nothing backs"
    assert shared_build_complete(drv_hash)[0] == "0", "hello's shared build is not complete to start with"

    refs = sql(
        f"SELECT cp.hash || '-' || cp.package FROM cached_path cp "
        f"WHERE cp.hash IN (SELECT split_part(t.tok, '-', 1) FROM cached_path r, "
        f"    unnest(string_to_array(r.\"references\", ' ')) AS t(tok) "
        f"    WHERE r.hash = '{store_hash}' AND length(t.tok) > 0) "
        f"  AND cp.hash <> '{store_hash}' AND cp.file_hash IS NOT NULL "
        f"ORDER BY (cp.package LIKE 'glibc-%') DESC, cp.package;"
    ).splitlines()

    def nar_object(name):
        h = name.split("-")[0]
        return f"/var/lib/gradient/nars/{h[:2]}/{h[2:]}.nar.zst"

    dep_name = next(
        (n for n in (name.strip() for name in refs)
         if n and server.execute(f"test -e /nix/store/{n}")[0] == 0),
        None,
    )
    assert dep_name, f"none of hello's {len(refs)} complete references is in the server store"
    dep_path = f"/nix/store/{dep_name}"
    dep_hash = dep_name.split("-")[0]
    dep_object = nar_object(dep_name)

    if server.execute(f"test -f {dep_object}")[0] != 0:
        print(f"{dep_path} is complete in the index with no local object; pushing it first")
        server.succeed(f"{CLI} cache upload main {dep_path}")
    server.succeed(f"test -f {dep_object}")

    siblings = sql(
        f"SELECT cp.hash || '-' || cp.package FROM derivation_output o "
        f"JOIN cached_path cp ON cp.hash = o.hash "
        f"WHERE o.derivation = (SELECT derivation FROM derivation_output "
        f"                      WHERE hash = '{dep_hash}') AND o.hash <> '{dep_hash}';"
    ).splitlines()
    for sibling in (s.strip() for s in siblings):
        if not sibling or server.execute(f"test -f {nar_object(sibling)}")[0] == 0:
            continue
        if server.execute(f"nix-store --check-validity /nix/store/{sibling} >/dev/null 2>&1")[0] != 0:
            print(f"{sibling} shares the producer, has no object and the store cannot realize its path; "
                  f"the purge will take it too")
            continue
        print(f"{sibling} shares the victim's producer with no object; pushing it first")
        server.succeed(f"{CLI} cache upload main /nix/store/{sibling}")

    print(f"retiring {dep_path}")
    indexed_before = set(sql("SELECT hash FROM cached_path;").split())
    server.succeed(f"rm {dep_object}")

    server.succeed(
        f"{CURL} -sf -X POST -H 'Authorization: Bearer {token}' "
        f"{API}/admin/maintenance/deep-gc"
    )
    poll(f"SELECT count(*) FROM cached_path WHERE hash = '{dep_hash}';", "0",
         "the zombie purge kept a row whose NAR is gone")
    purged = indexed_before - set(sql("SELECT hash FROM cached_path;").split())
    assert dep_hash in purged, f"the purge took {sorted(purged)}, not the victim {dep_hash}"
    bystanders = sorted(purged - set([dep_hash]))
    if bystanders:
        print(f"the purge also took {len(bystanders)} rows that were already zombies: {bystanders}")

    assert runtime_drift() == 0, "complete-closure counter disagrees with its recount after the retire"
    producer = sql(
        f"SELECT db.status::text || ' ' || db.fetchable::int::text FROM derivation_build db "
        f"JOIN derivation_output o ON o.derivation = db.derivation "
        f"WHERE o.hash = '{dep_hash}' LIMIT 1;"
    )
    assert producer, f"no shared build produces the retired output {dep_hash}; the check would pass on nothing"
    p_status, p_fetchable = producer.split()
    assert p_fetchable == "0" and p_status not in ("3", "7"), (
        f"the retired output's producer must lose fetchability and its terminal-success "
        f"status, has (status fetchable) = ({producer})"
    )
    hello_blocking = int(sql(
        f"SELECT db.blocking_deps FROM derivation_build db JOIN derivation d ON d.id = db.derivation "
        f"WHERE d.hash = '{drv_hash}';"
    ))
    assert hello_blocking >= 1, f"hello must count its unfetchable dependencies as blocking: {hello_blocking}"

    hello_missing, hello_fetchable = shared_build_complete(drv_hash)
    assert int(hello_missing) >= 1, (
        f"hello's shared build must count the retired reference as a missing runtime dep, "
        f"has {hello_missing}")
    assert hello_fetchable == "0", "a shared build that is not complete must not be fetchable"

    hello_build = sql(
        f"SELECT db.status::text || ' ' || db.fetchable::int::text FROM derivation_build db "
        f"JOIN derivation d ON d.id = db.derivation WHERE d.hash = '{drv_hash}';"
    )
    h_status, h_fetchable = hello_build.split()
    assert h_fetchable == "0" and h_status in ("3", "7"), (
        f"a parent that only lost its complete closure must keep its terminal-success status, or "
        f"the next evaluation rebuilds an output that never left the cache, has "
        f"(status fetchable) = ({hello_build})")
    assert shared_build_drift() == 0, "shared build counters disagree with their recount after the retire"

    print(server.succeed(f"{CLI} cache upload main {dep_path}"))
    assert runtime_drift() == 0, "complete-closure counter disagrees with its recount after the re-upload"
    assert shared_build_drift() == 0, "shared build counters disagree with their recount after the re-upload"
    poll(f"SELECT missing_runtime_deps FROM derivation_build db "
         f"JOIN derivation d ON d.id = db.derivation WHERE d.hash = '{drv_hash}';", "0",
         "the re-upload did not ripple hello's shared build back to complete")

    settled = sql(
        f"SELECT db.status::text || ' ' || db.fetchable::int::text FROM derivation_build db "
        f"JOIN derivation_output o ON o.derivation = db.derivation "
        f"WHERE o.hash = '{dep_hash}' LIMIT 1;"
    )
    s_status, s_fetchable = settled.split()
    assert s_fetchable == "0" or s_status in ("3", "7"), (
        f"the re-uploaded output's producer is fetchable again with no terminal-success "
        f"status, so the NAR alone re-trusted it; (status fetchable) = ({settled})")

    poll("SELECT count(*) FROM dispatched_job WHERE finished_at IS NULL "
         "AND dispatched_at > (now() AT TIME ZONE 'UTC') - interval '10 minutes';",
         "0", "a re-dispatched build is still running", timeout=300)
    assert runtime_drift() == 0, "complete-closure counter disagrees with its recount after the self-heal"
    assert shared_build_drift() == 0, "shared build counters disagree with their recount after the self-heal"

    banner("Phase 10d: pg_stat_statements bills the run")

    def psql_table(query):
        server.succeed(f"cat > /tmp/q.sql <<'EOF'\n{query}\nEOF")
        return server.succeed("su postgres -c 'psql -v ON_ERROR_STOP=1 -d gradient -f /tmp/q.sql'")

    server_statements = (
        "FROM pg_stat_statements s JOIN pg_roles r ON r.oid = s.userid "
        "WHERE r.rolname <> 'postgres' "
    )
    print(psql_table(
        "SELECT regexp_replace(substring(s.query, 1, 250), '[[:space:]]+', ' ', 'g') AS query, "
        "s.calls, round(s.total_exec_time::numeric, 2) AS total_time, "
        "round(s.mean_exec_time::numeric, 2) AS mean_time, "
        "round((100 * s.total_exec_time / sum(s.total_exec_time) OVER ())::numeric, 2) AS percentage "
        + server_statements +
        "ORDER BY s.total_exec_time DESC LIMIT 10;"
    ))

    counter_rows = int(sql(
        f"SELECT coalesce(sum(s.rows), 0) FROM pg_stat_statements s WHERE {COUNTER_WRITES};"
    )) - counter_rows_before
    build_rows = int(sql("SELECT count(*) FROM derivation_build;"))
    assert 0 < counter_rows <= build_rows, (
        f"one retire and one re-upload wrote {counter_rows} counter rows over a "
        f"{build_rows}-build graph; a moved counter touches parents, a derived one the table"
    )

    total_ms = float(sql(
        f"SELECT round(coalesce(sum(s.total_exec_time), 0)::numeric, 2) {server_statements};"
    ))
    counter_share = float(sql(
        f"SELECT round(coalesce(100 * sum(s.total_exec_time) FILTER (WHERE {COUNTER_WRITES}) "
        f"/ nullif(sum(s.total_exec_time), 0), 0)::numeric, 2) {server_statements};"
    ))
    print(f"server database time: {total_ms} ms, complete-closure counter writes: {counter_share}%")
    assert total_ms < 300000, f"the run burned {total_ms} ms of database time"
    assert counter_share < 50, f"maintaining the counter is {counter_share}% of database time"

    banner("Phase 10e: the next evaluation finds every output complete and settles the graph")

    poll("SELECT count(*) FROM dispatched_job WHERE finished_at IS NULL "
         "AND dispatched_at > (now() AT TIME ZONE 'UTC') - interval '10 minutes';",
         "0", "a re-dispatched build is still running", timeout=300)
    builds_before = int(sql("SELECT count(*) FROM dispatched_job WHERE kind = 1;"))

    server.succeed(f"{GIT} -C /var/lib/git/test commit --allow-empty -m 'retrigger'")
    server.succeed("chown git:git -R /var/lib/git/test")
    eval3_id = ""
    for attempt in range(1, 91):
        server.sleep(10)
        candidate = server.succeed(
            f'{CURL} -sf -H "Authorization: Bearer {token}" '
            f'{API}/tasks/project/task | {JQ} -rj ".message.last_evaluation // empty"'
        ).strip()
        status3 = ""
        if candidate and candidate not in (eval_id, eval2_id):
            status3 = server.succeed(
                f'{CURL} -sf -H "Authorization: Bearer {token}" '
                f'{API}/evals/{candidate} | {JQ} -rj ".message.status"'
            ).strip()
            if status3 == "Completed":
                eval3_id = candidate
                break
            if status3 == "Failed":
                j = server.succeed("journalctl -u gradient-server --no-pager --since='-600s' -n 300")
                raise Exception(f"the re-evaluation failed:\n{j[-3000:]}")
        if attempt % 3 == 0:
            print(f"  [{attempt:>2}/90] re-eval candidate={candidate or 'none'} "
                  f"status={status3 or '-'}")
    if not eval3_id:
        evals = sql(
            "SELECT e.id::text || ' status=' || e.status::text"
            " || ' created=' || e.created_at::text"
            " FROM evaluation e ORDER BY e.created_at DESC LIMIT 10;"
        )
        decided = server.succeed(
            "journalctl -u gradient-server --no-pager --since='-900s' -n 8000"
            " | grep -E 'skipping|update needed|Force evaluation|trigger created' "
            " | tail -n 15"
        )
        blocked = blocking_shared_builds(candidate) if candidate else "(no candidate)"
        raise Exception(
            f"the re-evaluation did not complete after 900 s. "
            f"task.last_evaluation={candidate or 'none'}, "
            f"eval1={eval_id}, eval2={eval2_id}\n"
            f"evaluations, newest first:\n{evals}\n"
            f"what it still waits on, by gate:\n{blocked}\n"
            f"what the trigger decided:\n{decided}"
        )

    producer_state = (
        f"SELECT db.status::text || ' ' || db.fetchable::int::text FROM derivation_build db "
        f"JOIN derivation_output o ON o.derivation = db.derivation "
        f"WHERE o.hash = '{dep_hash}' LIMIT 1;"
    )
    for _ in range(60):
        producer = sql(producer_state)
        if producer in ("3 1", "7 1"):
            break
        server.sleep(10)
    else:
        unbacked = sql(
            f"SELECT count(*) FROM derivation_output o "
            f"LEFT JOIN cached_path cp ON cp.hash = o.hash "
            f"WHERE o.derivation = (SELECT derivation FROM derivation_output "
            f"                      WHERE hash = '{dep_hash}') AND cp.hash IS NULL;"
        )
        raise Exception(
            f"the retired output's producer never became terminal-success and fetchable "
            f"in 600 s, has (status fetchable) = ({producer}) with {unbacked} of its "
            f"outputs missing a cached_path row"
        )
    hello_blocking = (
        f"SELECT db.blocking_deps FROM derivation_build db "
        f"JOIN derivation d ON d.id = db.derivation WHERE d.hash = '{drv_hash}';"
    )
    for _ in range(12):
        if sql(hello_blocking) == "0":
            break
        server.sleep(5)
    else:
        raise Exception(
            f"hello's counter must return to zero, is {sql(hello_blocking)} with these "
            f"unfetchable inputs: " + sql(
                f"SELECT string_agg(d.name || ' status=' || dep.status::text "
                f"  || ' fetchable=' || dep.fetchable::int::text "
                f"  || ' blocking=' || dep.blocking_deps::text, ', ') "
                f"FROM derivation_dependency e "
                f"JOIN derivation_build dep ON dep.derivation = e.dependency "
                f"JOIN derivation d ON d.id = e.dependency "
                f"WHERE e.derivation = (SELECT id FROM derivation WHERE hash = '{drv_hash}') "
                f"  AND e.kind IN (0, 2) AND NOT dep.fetchable;"
            )
        )
    unsettled = int(sql(
        f"SELECT count(*) FROM build_job bj "
        f"JOIN derivation_build db ON db.derivation = bj.derivation "
        f"WHERE bj.evaluation = '{eval3_id}' AND (db.status NOT IN (3, 7) OR NOT db.fetchable);"
    ))
    assert unsettled == 0, f"{unsettled} shared builds of the completed re-evaluation are not settled and fetchable"
    assert runtime_drift() == 0, "complete-closure counter disagrees with its recount after the re-evaluation"
    assert shared_build_drift() == 0, "shared build counters disagree with their recount after the re-evaluation"

    builds_after = int(sql("SELECT count(*) FROM dispatched_job WHERE kind = 1;"))
    print(f"builds dispatched around the re-evaluation: {builds_after - builds_before}")

    churn = sql(
        "SELECT string_agg(d.name || ' built ' || x.builds::text || ' times', ', ') "
        "FROM (SELECT ba.derivation_build, count(*) AS builds FROM build_attempt ba "
        "      WHERE NOT ba.substitute AND ba.outcome IN (1, 2) "
        "      GROUP BY ba.derivation_build HAVING count(*) > 2) x "
        "JOIN derivation_build db ON db.id = x.derivation_build "
        "JOIN derivation d ON d.id = db.derivation;"
    )
    assert churn == "", f"a build-once shared build was rebuilt past the one granted retry: {churn}"

    banner("Phase 10f: a retire holds its locks while a can-start mark waits")
    LR_DRV = "aaaaaaaa-0000-4000-8000-00000000fe01"
    LR_DRV2 = "aaaaaaaa-0000-4000-8000-00000000fe02"
    lr_drv_hash = "lockraced".ljust(32, "0")
    lr_out_hash = "lockraceo".ljust(32, "0")
    lr_drv2_hash = "lockracee".ljust(32, "0")
    lr_out2_hash = "lockracep".ljust(32, "0")

    def lockrace_cleanup(drv, out_hash):
        sql(
            f"DELETE FROM cached_path WHERE hash = '{out_hash}';\n"
            f"DELETE FROM derivation_output WHERE derivation = '{drv}';\n"
            f"DELETE FROM derivation_build WHERE derivation = '{drv}';\n"
            f"DELETE FROM derivation WHERE id = '{drv}';"
        )

    server.succeed(f"cat > /tmp/lockrace.sh <<'LOCKRACE'\n{LOCK_RACE_SH}\nLOCKRACE")
    print(server.succeed(
        f"LR_DRV={LR_DRV} LR_DRV_HASH={lr_drv_hash} LR_OUT={lr_out_hash} "
        f"LR_DRV2={LR_DRV2} LR_DRV2_HASH={lr_drv2_hash} LR_OUT2={lr_out2_hash} "
        f"sh /tmp/lockrace.sh"
    ))

    lockrace_cleanup(LR_DRV, lr_out_hash)
    lockrace_cleanup(LR_DRV2, lr_out2_hash)
    race_drift = runtime_drift()
    race_shared_build_drift = shared_build_drift()
    assert race_drift == 0 and race_shared_build_drift == 0, (
        f"counters disagree with their recount after the lock race "
        f"(complete closure {race_drift}, can-start {race_shared_build_drift})"
    )

    banner("Phase 10g: need-driven substitution (#593)")

    server.succeed(
        f"{NIX} --extra-experimental-features 'nix-command flakes' copy "
        f"--to 'file:///srv/upstream?secret-key=/etc/gradient/secrets/upstream_key' --no-check-sigs "
        f"${pkgs.busybox.out} ${pkgs.busybox.debug}"
    )
    server.succeed("chown -R nginx:nginx /srv/upstream && systemctl reload nginx")
    server.succeed(f"{CURL} -sf http://gradient.local/upstream/nix-cache-info > /dev/null")
    builder.succeed(f"{CURL} -sf http://server/upstream/nix-cache-info > /dev/null")

    assert "file-upstream" in api_get(token, "caches/main/upstream-caches"), "the declared upstream was not provisioned"

    def shared_build_of(drv):
        """`<status> <cache_available> <passthrough attempts>` of `drv`'s shared build."""
        return sql(shared_build_column(
            drv,
            "db.status::text || ' ' || db.cache_available::int::text || ' ' || "
            "(SELECT count(*) FROM build_attempt a "
            " WHERE a.derivation_build = db.id AND a.substitute)::text",
        ))

    def shared_build_column(drv, column):
        return f"SELECT {column} FROM derivation_build db WHERE db.derivation = '{drv}';"

    def passthrough_attempts(drv):
        return shared_build_column(
            drv,
            "(SELECT count(*) FROM build_attempt a "
            " WHERE a.derivation_build = db.id AND a.substitute)::text",
        )

    def output_missing(drv):
        """The shared build's missing runtime dependencies: zero means the whole closure landed."""
        return sql(shared_build_column(drv, "db.missing_runtime_deps::text"))

    def output_hash(drv):
        return sql(
            f"SELECT o.hash FROM derivation_output o "
            f"WHERE o.derivation = '{drv}' AND o.name = 'out';"
        )

    def wait_for_new_eval(known, timeout=900):
        """The id of the next evaluation to reach a terminal status."""
        candidate = ""
        for _ in range(timeout // 10):
            server.sleep(10)
            candidate = server.succeed(
                f'{CURL} -sf -H "Authorization: Bearer {token}" '
                f'{API}/tasks/project/task | {JQ} -rj ".message.last_evaluation // empty"'
            ).strip()
            if not candidate or candidate in known:
                continue
            status = server.succeed(
                f'{CURL} -sf -H "Authorization: Bearer {token}" '
                f'{API}/evals/{candidate} | {JQ} -rj ".message.status"'
            ).strip()
            if status == "Completed":
                return candidate
            if status in ("Failed", "Aborted"):
                j = server.succeed("journalctl -u gradient-server --no-pager --since='-600s' -n 300")
                raise Exception(f"evaluation {candidate} ended {status}:\n{j[-3000:]}")
        blocked = blocking_shared_builds(candidate) if candidate else "(no candidate)"
        edges = blocking_reasons(candidate) if candidate else ""
        failed = sql(
            f"SELECT d.name || ' ' || db.status::text FROM derivation_build db"
            f" JOIN derivation d ON d.id = db.derivation"
            f" JOIN build_job bj ON bj.derivation_build = db.id"
            f" WHERE bj.evaluation = '{candidate}' AND db.status IN (4, 6, 9)"
            f" ORDER BY 1 LIMIT 40;"
        ) if candidate else ""
        raise Exception(
            f"no new evaluation completed within {timeout} s. candidate={candidate or 'none'}\n"
            f"what it still waits on, by gate:\n{blocked}\n"
            f"the edges those shared builds count as blocking:\n{edges}\n"
            f"terminal failures in the same evaluation:\n{failed}"
        )

    server.succeed("cp /var/lib/git/flake-busywrap.nix /var/lib/git/test/flake.nix")
    server.succeed("sed -i 's#\\[nixpkgs\\]#${self.inputs.nixpkgs}#g' /var/lib/git/test/flake.nix")
    server.succeed(f"{GIT} -C /var/lib/git/test commit -am 'busywrap'")
    server.succeed("chown git:git -R /var/lib/git/test")
    eval4_id = wait_for_new_eval({eval_id, eval2_id, eval3_id})

    busybox = sql("SELECT id FROM derivation WHERE name = '${pkgs.busybox.name}';")
    busywrap = sql("SELECT id FROM derivation WHERE name = 'busywrap';")
    assert busybox, "the evaluation walked no derivation named ${pkgs.busybox.name}"
    assert busywrap, "the evaluation walked no derivation named busywrap"

    assert shared_build_of(busywrap).startswith("3 0"), (
        f"busywrap must be built here, not passed through: {shared_build_of(busywrap)}"
    )
    assert shared_build_of(busybox) == "7 1 1", (
        f"busybox must be passed through exactly once off the upstream: {shared_build_of(busybox)}"
    )
    assert output_missing(busybox) == "0", (
        f"busybox's passed-through output is missing closure members: {output_missing(busybox)}"
    )
    passthrough_inputs_built = sql(
        "SELECT count(*) FROM build_attempt a "
        "JOIN derivation_build db ON db.id = a.derivation_build "
        "JOIN derivation d ON d.id = db.derivation "
        "WHERE d.name LIKE 'unzip60%' OR d.name LIKE 'CVE-2019-13232%' "
        "   OR d.name LIKE 'patchutils%';"
    )
    assert passthrough_inputs_built == "0", (
        f"a passthrough's inputs were built anyway: {passthrough_inputs_built} attempts"
    )
    sources, built, statuses = sql(
        f"SELECT count(*)::text || ' ' || count(*) FILTER ("
        f"  WHERE db.status = 3 OR (NOT (db.cache_available OR db.substituted) AND EXISTS ("
        f"    SELECT 1 FROM build_attempt a WHERE a.derivation_build = db.id)))::text "
        f"|| ' ' || coalesce(string_agg(DISTINCT db.status::text, ','), '-') "
        f"FROM derivation_build db JOIN derivation d ON d.id = db.derivation "
        f"JOIN derivation_dependency e ON e.dependency = d.id AND e.derivation = '{busybox}' "
        f"WHERE d.name LIKE '%.tar%';"
    ).split()
    assert int(sources) >= 1 and built == "0", (
        f"busybox's source may be passed through, never built: "
        f"{sources} sources, {built} built, status {statuses}"
    )
    assert runtime_drift() == 0, "complete-closure counter disagrees with its recount after the passthrough"
    assert shared_build_drift() == 0, "shared build counters disagree with their recount after the passthrough"
    assert wanted_drift() == 0, "wanted flag disagrees with its recount after the passthrough"

    server.succeed(f"{GIT} -C /var/lib/git/test commit --allow-empty -m 'busywrap again'")
    server.succeed("chown git:git -R /var/lib/git/test")
    eval5_id = wait_for_new_eval({eval_id, eval2_id, eval3_id, eval4_id})
    assert shared_build_of(busybox) == "7 1 1", (
        f"the second evaluation passed a complete shared build through again: {shared_build_of(busybox)}"
    )
    poll(
        f"SELECT (count(*) > 0)::text FROM evaluation e JOIN task t ON t.id = e.task "
        f"JOIN commit c ON c.id = e.commit "
        f"WHERE t.name = 'task2' AND e.status = 5 AND c.hash = ("
        f"  SELECT c5.hash FROM evaluation e5 JOIN commit c5 ON c5.id = e5.commit "
        f"  WHERE e5.id = '{eval5_id}');",
        "true", "task2 never completed its evaluation of the 'busywrap again' commit",
        timeout=600,
    )

    bb_hash = output_hash(busybox)
    assert bb_hash, "busybox has no cached output row to retire"
    server.succeed(f"rm -f {nar_object(bb_hash)}")
    server.succeed(
        f"{CURL} -sf -X POST -H 'Authorization: Bearer {token}' "
        f"{API}/admin/maintenance/deep-gc"
    )
    poll(f"SELECT count(*) FROM cached_path WHERE hash = '{bb_hash}';", "0",
         "the zombie purge kept busybox's row after its NAR was deleted")
    poll(shared_build_column(busybox, "db.status::text"), "0",
         "the retire left busybox terminal-success with nothing to serve")

    server.sleep(45)
    assert shared_build_of(busybox) == "0 1 1", (
        f"an unwanted passthrough was dispatched again: {shared_build_of(busybox)}"
    )

    bw_hash = output_hash(busywrap)
    assert bw_hash, "busywrap has no cached output row to retire"
    server.succeed(f"rm -f {nar_object(bw_hash)}")
    server.succeed(
        f"{CURL} -sf -X POST -H 'Authorization: Bearer {token}' "
        f"{API}/admin/maintenance/deep-gc"
    )
    poll(f"SELECT count(*) FROM cached_path WHERE hash = '{bw_hash}';", "0",
         "the zombie purge kept busywrap's row after its NAR was deleted")
    poll(passthrough_attempts(busybox), "2",
         "a wanted passthrough was not re-dispatched after its NAR was retired",
         timeout=600)
    poll(shared_build_column(busywrap, "(db.status IN (3, 7))::text"), "true",
         "busywrap did not come back once its input was passed through again", timeout=600)
    poll(f"SELECT count(*) FROM cached_path WHERE hash = '{bw_hash}' AND file_hash IS NOT NULL;", "1",
         "busywrap's output was not pushed back to the cache")
    assert output_missing(busybox) == "0", (
        f"the second passthrough left closure members behind: {output_missing(busybox)}"
    )
    assert runtime_drift() == 0, "complete-closure counter disagrees with its recount after the second passthrough"
    assert shared_build_drift() == 0, "shared build counters disagree with their recount after the second passthrough"
    assert wanted_drift() == 0, "wanted flag disagrees with its recount after the second passthrough"

    banner("Phase 10h: a pruned interior is adopted, attributed and rebuilt (#663)")

    hello_drv = sql(f"SELECT id FROM derivation WHERE hash = '{drv_hash}';")
    assert hello_drv, "hello's derivation row is gone"

    def built_input_of(derivation, needs_inputs):
        """The least-shared direct input that is a walked, non-cache_available,
        terminal-success builder with complete outputs and a complete .drv: startable to
        be queued the moment it is reset."""
        inputs = (
            "AND EXISTS (SELECT 1 FROM derivation_dependency i WHERE i.derivation = e.dependency) "
            if needs_inputs else ""
        )
        return sql(
            f"SELECT e.dependency FROM derivation_dependency e "
            f"JOIN derivation_build db ON db.derivation = e.dependency "
            f"JOIN derivation d ON d.id = e.dependency "
            f"JOIN cached_path drv ON drv.hash = d.hash "
            f"WHERE e.derivation = '{derivation}' AND d.walked AND NOT db.cache_available "
            f"  AND db.status IN (3, 7) AND db.fetchable AND db.blocking_deps = 0 "
            f"  AND drv.file_hash IS NOT NULL {inputs}"
            f"ORDER BY (SELECT count(*) FROM derivation_dependency r WHERE r.dependency = e.dependency), "
            f"         d.name LIMIT 1;"
        )

    d1 = built_input_of(hello_drv, needs_inputs=True)
    d2 = built_input_of(d1, needs_inputs=False) if d1 else ""
    assert d1 and d2, f"hello has no two-deep chain of rebuildable inputs (d1={d1!r}, d2={d2!r})"
    chain = f"'{hello_drv}', '{d1}', '{d2}'"
    print("chain under test: " + sql(
        f"SELECT string_agg(d.name, ' > ' ORDER BY d.id = '{hello_drv}' DESC, d.id = '{d1}' DESC) "
        f"FROM derivation d WHERE d.id IN ({chain});"
    ))
    outputs = sql(f"SELECT o.hash FROM derivation_output o WHERE o.derivation IN ({chain});").split()
    assert len(outputs) >= 3, f"the chain has only {len(outputs)} outputs"

    sql(f"DELETE FROM build_job WHERE derivation IN ('{d1}', '{d2}');")
    assert sql(f"SELECT count(*) FROM build_job WHERE derivation IN ('{d1}', '{d2}');") == "0"

    for h in outputs:
        server.succeed(f"rm -f {nar_object(h)}")
    server.succeed(
        f"{CURL} -sf -X POST -H 'Authorization: Bearer {token}' "
        f"{API}/admin/maintenance/deep-gc"
    )
    for h in outputs:
        poll(f"SELECT count(*) FROM cached_path WHERE hash = '{h}';", "0",
             f"the zombie purge kept {h}'s row after its NAR was deleted")
    poll(f"SELECT count(*) FROM derivation_build WHERE derivation IN ({chain}) AND status = 0;", "3",
         "the retire did not reset every producer of the chain")
    assert runtime_drift() == 0, "complete-closure counter disagrees with its recount after the retire"
    assert shared_build_drift() == 0, "shared build counters disagree with their recount after the retire"

    server.sleep(20)
    assert sql(f"SELECT status::text FROM derivation_build WHERE derivation = '{d2}';") == "0", (
        "an interior with no name was queued before anything adopted it"
    )

    newest_task2 = (
        "SELECT e.id FROM evaluation e JOIN task t ON t.id = e.task "
        "WHERE t.name = 'task2' ORDER BY e.created_at DESC LIMIT 1"
    )
    poll(f"SELECT count(*) FROM build_job bj WHERE bj.derivation = '{hello_drv}' "
         f"AND bj.evaluation = ({newest_task2});",
         "1", "task2's newest evaluation never named the builder the adoption walks out of",
         timeout=420)
    task2_eval = sql(f"{newest_task2};")
    assert task2_eval, "task2 has no evaluation left to build against the subtree"
    sql(f"UPDATE evaluation SET status = 3, "
        f"building_started_at = (now() AT TIME ZONE 'UTC'), updated_at = (now() AT TIME ZONE 'UTC') "
        f"WHERE id = '{task2_eval}';")
    assert sql(
        f"SELECT count(*) FROM build_job bj WHERE bj.evaluation = '{task2_eval}' "
        f"AND bj.derivation = '{hello_drv}';"
    ) == "1", "task2's evaluation does not name the builder the adoption walks out of"

    poll(f"SELECT count(*) FROM build_job WHERE evaluation = '{task2_eval}' AND derivation IN ('{d1}', '{d2}');",
         "2", "task2's evaluation did not adopt the interior it never walked", timeout=420)
    status2 = ""
    for _ in range(60):
        status2 = server.succeed(
            f'{CURL} -sf -H "Authorization: Bearer {token}" '
            f'{API}/evals/{task2_eval} | {JQ} -rj ".message.status"'
        ).strip()
        if status2 == "Completed":
            break
        if status2 in ("Failed", "Aborted"):
            j = server.succeed("journalctl -u gradient-server --no-pager --since='-600s' -n 300")
            raise Exception(f"task2's evaluation ended {status2} over the adopted chain:\n{j[-3000:]}")
        server.sleep(10)
    else:
        raise Exception(f"task2's evaluation did not complete over the adopted chain in 600 s (still {status2!r})")

    assert sql(
        f"SELECT count(*) FROM derivation_build WHERE derivation IN ({chain}) AND status IN (3, 7) AND fetchable;"
    ) == "3", "the chain is not terminal-success and fetchable again"
    for h in outputs:
        assert sql(
            f"SELECT count(*) FROM cached_path WHERE hash = '{h}' AND file_hash IS NOT NULL;"
        ) == "1", f"{h} did not come back"
    assert sql(
        f"SELECT count(*) FROM derivation_build WHERE derivation IN ({chain}) "
        f"AND missing_runtime_deps = 0;"
    ) == "3", "the chain did not come back complete"
    attributed = int(sql(
        f"SELECT count(DISTINCT bj.derivation) FROM build_attempt a JOIN build_job bj ON bj.id = a.build_job "
        f"WHERE bj.evaluation = '{task2_eval}' AND bj.derivation IN ('{d1}', '{d2}');"
    ))
    assert attributed == 2, f"the interior's rebuilds were attributed to {attributed} of the 2 adopted names"
    assert runtime_drift() == 0, "complete-closure counter disagrees with its recount after the adopted rebuild"
    assert shared_build_drift() == 0, "shared build counters disagree with their recount after the adopted rebuild"
    print(server.succeed("journalctl -u gradient-server --no-pager | grep -i 'adopt' | tail -n 5"))

    banner("Phase 10l: an aborted evaluation stops evaluating on the worker")

    def eval_cpu_ticks(node):
        out = node.succeed(
            "for p in $(pgrep -f -- --eval-subprocess); do "
            "cut -d' ' -f14,15 /proc/$p/stat 2>/dev/null; done; true"
        )
        return sum(int(t) for t in out.split())

    def evaluating_nodes():
        before = {node.name: eval_cpu_ticks(node) for node, _ in fleet_units()}
        server.sleep(2)
        return [node.name for node, _ in fleet_units()
                if eval_cpu_ticks(node) - before[node.name] > 100]

    known = sql("SELECT string_agg(id::text, ',') FROM evaluation;")
    spin_evals = f"FROM evaluation WHERE NOT (id = ANY(string_to_array('{known}', ',')::uuid[]))"
    abort_since = server.succeed("date '+%F %T'").strip()
    server.succeed("cp /var/lib/git/flake-spin.nix /var/lib/git/test/flake.nix")
    server.succeed("sed -i 's#\\[nixpkgs\\]#${self.inputs.nixpkgs}#g' /var/lib/git/test/flake.nix")
    server.succeed(f"{GIT} -C /var/lib/git/test commit -am 'spin'")
    server.succeed("chown git:git -R /var/lib/git/test")

    poll(f"SELECT count(*) {spin_evals};", "2", "both tasks did not pick up the spin commit")
    poll(f"SELECT (count(*) > 0)::text {spin_evals} AND status = 2;",
         "true", "no spin evaluation reached its derivation phase", timeout=300)
    for _ in range(60):
        if evaluating_nodes():
            break
    else:
        raise Exception("no worker is evaluating the spin flake")

    for evaluation in sql(f"SELECT id {spin_evals};").splitlines():
        server.succeed(
            f'{CURL} -sf -X POST -H "Authorization: Bearer {token}" '
            f'-H "Content-Type: application/json" -d \'{{"method": "abort"}}\' '
            f'{API}/evals/{evaluation}'
        )

    deadline = time.time() + 60
    while (busy := evaluating_nodes()) and time.time() < deadline:
        pass
    if busy:
        logs = "\n".join(
            node.succeed(f"journalctl -u {unit} --no-pager --since='{abort_since}' | grep -i abort || true")
            for node, unit in fleet_units()
        )
        raise Exception(f"{busy} still evaluating 60 s after the abort:\n{logs}")

    assert sql(f"SELECT string_agg(DISTINCT status::text, ',') {spin_evals};") == "7", (
        "a spin evaluation left Aborted after its worker stopped"
    )
    server.fail(
        f"journalctl -u gradient-server --no-pager --since='{abort_since}' "
        "| grep -q 'worker never confirmed the abort'"
    )

    banner("Phase 10m: a build aborted and retried in a running evaluation (#673)")

    def build_action(build_job, action):
        return server.succeed(
            f'{CURL} -s -o /dev/null -w "%{{http_code}}" -X POST '
            f'-H "Authorization: Bearer {token}" {API}/builds/{build_job}/{action}'
        ).strip()

    known = sql("SELECT string_agg(id::text, ',') FROM evaluation;")
    slow_evals = f"SELECT id FROM evaluation WHERE NOT (id = ANY(string_to_array('{known}', ',')::uuid[]))"
    server.succeed("cp /var/lib/git/flake-slow.nix /var/lib/git/test/flake.nix")
    server.succeed("sed -i 's#\\[nixpkgs\\]#${self.inputs.nixpkgs}#g' /var/lib/git/test/flake.nix")
    server.succeed(f"{GIT} -C /var/lib/git/test commit -am 'slow'")
    server.succeed("chown git:git -R /var/lib/git/test")

    def named(name, evaluations):
        return (
            "FROM build_job bj JOIN derivation d ON d.id = bj.derivation "
            "JOIN derivation_build db ON db.id = bj.derivation_build "
            f"WHERE d.name = '{name}' AND bj.evaluation IN ({evaluations})"
        )

    poll(f"SELECT count(*) {named('slow-leaf', slow_evals)};", "2",
         "both tasks did not name slow-leaf", timeout=600)
    poll(f"SELECT (db.status IN (1, 2))::text {named('slow-leaf', slow_evals)} LIMIT 1;", "true",
         "slow-leaf was never queued", timeout=600)

    first, second = sql(
        f"SELECT bj.evaluation || ' ' || bj.id {named('slow-leaf', slow_evals)} ORDER BY bj.evaluation;"
    ).splitlines()
    kept_eval, leaf_job = first.split()
    dropped_eval = second.split()[0]
    assert build_action(leaf_job, "abort") == "409", (
        "slow-leaf was aborted while the other task's evaluation still needs it"
    )

    server.succeed(
        f'{CURL} -sf -X POST -H "Authorization: Bearer {token}" '
        f'-H "Content-Type: application/json" -d \'{{"method": "abort"}}\' '
        f'{API}/evals/{dropped_eval}'
    )
    poll(f"SELECT status::text FROM evaluation WHERE id = '{dropped_eval}';", "7",
         "the other evaluation did not abort")
    assert build_action(leaf_job, "abort") == "200", "the build abort was refused"

    kept = f"'{kept_eval}'"

    def kept_status(name):
        return sql(f"SELECT db.status {named(name, kept)};")

    assert kept_status("slow-leaf") == "5", f"slow-leaf is {kept_status('slow-leaf')}, not Aborted"
    assert kept_status("slow-wrap") == "6", "the aborted build's dependent still waits"
    assert sql(f"SELECT (status NOT IN (5, 6, 7))::text FROM evaluation WHERE id = '{kept_eval}';") == "true", (
        "the evaluation ended while slow-keep still builds"
    )

    for _ in range(60):
        retried = build_action(leaf_job, "retry")
        if retried != "409":
            break
        server.sleep(1)
    assert retried == "200", f"the retry answered {retried}"
    assert kept_status("slow-leaf") in ("0", "1", "2"), f"slow-leaf is {kept_status('slow-leaf')} after the retry"
    assert kept_status("slow-wrap") in ("0", "1"), f"slow-wrap is {kept_status('slow-wrap')} after the retry"
    assert shared_build_drift() == 0, "shared build counters disagree with their recount after the retry"

    server.succeed(
        f'{CURL} -sf -X POST -H "Authorization: Bearer {token}" '
        f'-H "Content-Type: application/json" -d \'{{"method": "abort"}}\' '
        f'{API}/evals/{kept_eval}'
    )
    poll(f"SELECT status::text FROM evaluation WHERE id = '{kept_eval}';", "7",
         "the retried evaluation did not abort")

    banner("Phase 10i: live-closure retention (#594)")

    probe_object = nar_object(f"{upload_hash}-probe")
    assert sql(f"SELECT count(*) FROM cached_path WHERE hash = '{upload_hash}';") == "1", \
        "phase 9's probe left the cache before this phase could evict it"
    server.succeed(f"test -e {probe_object}")

    def age(hashes, hours):
        """Backdate the commit and the last fetch of `hashes` by `hours`."""
        listed = "', '".join(hashes)
        sql(f"UPDATE cached_path SET created_at = created_at - interval '{hours} hours' "
            f"WHERE hash IN ('{listed}');")
        sql(f"UPDATE cached_path_signature SET last_fetched_at = last_fetched_at - interval '{hours} hours' "
            f"WHERE cached_path IN (SELECT id FROM cached_path WHERE hash IN ('{listed}'));")

    age([upload_hash], 26)
    poll(f"SELECT count(*) FROM cached_path WHERE hash = '{upload_hash}';", "0",
         "the eviction kept a path no retained evaluation reaches", timeout=240)
    server.fail(f"test -e {probe_object}")
    assert sql(
        f"SELECT count(*) FROM cached_path WHERE hash IN ('{store_hash}', '{dep_hash}');"
    ) == "2", "the eviction took a path the live closure still reaches"
    assert runtime_drift() == 0, "complete-closure counter disagrees with its recount after the eviction"

    sql("DELETE FROM task_trigger;")
    hello_drv = sql(f"SELECT id FROM derivation WHERE hash = '{drv_hash}';")
    assert hello_drv, "hello's derivation row is gone before this phase deleted anything"
    hello_paths = [h for h in (store_hash, drv_hash)
                   if sql(f"SELECT count(*) FROM cached_path WHERE hash = '{h}';") == "1"]
    assert store_hash in hello_paths, "hello's output is not cached; the eviction would prove nothing"

    sql(f"DELETE FROM build_job WHERE derivation = '{hello_drv}';")
    sql(f"DELETE FROM entry_point WHERE derivation = '{hello_drv}';")
    sql(f"UPDATE derivation SET created_at = created_at - interval '48 hours' WHERE id = '{hello_drv}';")
    age(hello_paths, 26)

    poll(f"SELECT count(*) FROM derivation WHERE id = '{hello_drv}';", "0",
         "the orphan GC kept a derivation nothing reaches", timeout=240)
    listed = "', '".join(hello_paths)
    poll(f"SELECT count(*) FROM cached_path WHERE hash IN ('{listed}');", "0",
         "hello's paths outlived the only derivation that reached them", timeout=240)
    for h in hello_paths:
        server.fail(f"test -e {nar_object(h + '-hello')}")
    assert sql(f"SELECT count(*) FROM cached_path WHERE hash = '{dep_hash}';") == "1", \
        "a path its own name still reaches left the cache with hello"
    assert runtime_drift() == 0, "complete-closure counter disagrees with its recount after the GC and the eviction"

    banner("Phase 10i: a naming racing a transition still settles (#640)")
    CR_DRV = "1950ecc8-8530-4e1c-bc1c-de4bd759fe57"
    CR_HOLD_DRV = "5f923367-19cc-4823-9b73-5c4b9fe4d0ca"
    cr_eval = sql(
        "SELECT e.id FROM evaluation e WHERE e.status = 5 AND NOT EXISTS ("
        "SELECT 1 FROM build_job bj JOIN derivation_build db ON db.id = bj.derivation_build "
        "WHERE bj.evaluation = e.id AND db.status NOT IN (3, 7)) "
        "ORDER BY e.created_at LIMIT 1;"
    )
    assert cr_eval, "no cleanly completed evaluation to park for the race"

    def counterrace_shared_build(drv, name, status):
        sql(
            f"INSERT INTO derivation (id, created_at, architecture, hash, name, "
            f"prefer_local_build, allow_substitutes, is_fixed_output, walked) VALUES "
            f"('{drv}', now() AT TIME ZONE 'UTC', 'x86_64-linux', '{name.ljust(32, '0')}', "
            f"'{name}', false, true, false, false);\n"
            f"INSERT INTO derivation_build (id, derivation, status, cache_available, substituted, "
            f"fetchable, blocking_deps, attempt, wanted, created_at, updated_at) VALUES "
            f"(uuidv7(), '{drv}', {status}, false, false, false, 0, 0, true, "
            f"now() AT TIME ZONE 'UTC', now() AT TIME ZONE 'UTC');"
        )
        return sql(f"SELECT id FROM derivation_build WHERE derivation = '{drv}';")

    cr_build = counterrace_shared_build(CR_DRV, "counterrace", 0)
    cr_hold = counterrace_shared_build(CR_HOLD_DRV, "counterhold", 2)

    server.succeed(f"cat > /tmp/counterrace.sh <<'COUNTERRACE'\n{COUNTER_RACE_SH}\nCOUNTERRACE")
    print(server.succeed(
        f"CR_EVAL={cr_eval} CR_BUILD={cr_build} CR_HOLD={cr_hold} sh /tmp/counterrace.sh"
    ))

    poll(f"SELECT status FROM evaluation WHERE id = '{cr_eval}';", "5",
         "an evaluation whose counters kept a shared build the race hid never settled", timeout=120)
    agree = sql(
        "SELECT (e.named_shared_builds + coalesce((SELECT sum(d.named) FROM evaluation_shared_build_delta d WHERE d.evaluation = e.id), 0), "
        "e.active_shared_builds + coalesce((SELECT sum(d.active) FROM evaluation_shared_build_delta d WHERE d.evaluation = e.id), 0)) "
        "= (SELECT count(*), coalesce(sum(x.active), 0) FROM build_job bj "
        "JOIN derivation_build db ON db.id = bj.derivation_build "
        "CROSS JOIN LATERAL evaluation_shared_build_counts(db.status, db.wanted) x "
        f"WHERE bj.evaluation = e.id) FROM evaluation e WHERE e.id = '{cr_eval}';"
    )

    sql(
        f"DELETE FROM build_job WHERE derivation_build IN ('{cr_build}', '{cr_hold}');\n"
        f"DELETE FROM derivation_build WHERE id IN ('{cr_build}', '{cr_hold}');\n"
        f"DELETE FROM derivation WHERE id IN ('{CR_DRV}', '{CR_HOLD_DRV}');"
    )
    assert agree == "t", "the evaluation settled but its counters still disagree with their recount"

    banner("Phase 10j: two instances claiming one job, one wins (#641)")
    CL_DRV = "8ef77385-09cc-40f8-833e-732d330a2685"
    cl_owner = sql("SELECT evaluation_id || ' ' || project FROM dispatched_job LIMIT 1;")
    assert cl_owner, "no dispatched job to borrow an evaluation and project from"
    cl_eval, cl_project = cl_owner.split()
    sql(
        f"INSERT INTO derivation (id, created_at, architecture, hash, name, "
        f"prefer_local_build, allow_substitutes, is_fixed_output, walked) VALUES "
        f"('{CL_DRV}', now() AT TIME ZONE 'UTC', 'x86_64-linux', '{'claimrace'.ljust(32, '0')}', "
        f"'claimrace', false, true, false, true);\n"
        f"INSERT INTO derivation_build (id, derivation, status, cache_available, substituted, "
        f"fetchable, blocking_deps, attempt, wanted, created_at, updated_at) VALUES "
        f"(uuidv7(), '{CL_DRV}', 1, false, false, false, 0, 0, true, "
        f"now() AT TIME ZONE 'UTC', now() AT TIME ZONE 'UTC');"
    )
    cl_build = sql(f"SELECT id FROM derivation_build WHERE derivation = '{CL_DRV}';")

    server.succeed(f"cat > /tmp/claimrace.sh <<'CLAIMRACE'\n{CLAIM_RACE_SH}\nCLAIMRACE")
    out = server.succeed(
        f"CL_BUILD={cl_build} CL_EVAL={cl_eval} CL_PROJECT={cl_project} sh /tmp/claimrace.sh"
    )
    print(out)
    open_rows = sql(
        f"SELECT count(*) FROM dispatched_job WHERE job_id = 'build:{cl_build}' AND finished_at IS NULL;"
    )

    sql(
        f"DELETE FROM dispatched_job WHERE job_id = 'build:{cl_build}';\n"
        f"DELETE FROM derivation_build WHERE id = '{cl_build}';\n"
        f"DELETE FROM derivation WHERE id = '{CL_DRV}';"
    )
    assert "claimrace_a INSERT 0 1" in out, f"the first claim did not win: {out}"
    assert "claimrace_b INSERT 0 0" in out, f"the second claim inserted a duplicate: {out}"
    assert open_rows == "1", f"{open_rows} open rows for one job key"

    banner("Phase 10k: a seed and a flip see each other through the shared build keys (#643)")
    server.succeed(f"cat > /tmp/lockguard.sh <<'LOCKGUARD'\n{LOCK_GUARD_SH}\nLOCKGUARD")
    out = server.succeed(
        "LG_PSQL='su postgres -c \"psql -X -At -d gradient\"' "
        "LG_GATE=${pkgs.gradient.gate}/bin/gradient-sql-gate bash /tmp/lockguard.sh 2>&1"
    )
    print(out)
    for arm in ("flip-first", "seed-first", "incomplete", "retire-reads", "shared"):
        assert f"lockguard {arm}: recount wrote 0" in out, f"arm {arm} drifted:\n{out}"
    assert "lockguard unguarded: recount wrote 1" in out, (
        f"the unguarded arm did not drift, so the keys are not what the others prove:\n{out}"
    )

    banner("Phase 11: every supervised loop is running; SIGTERM stops the server")
    health = json.loads(api_get(token, "board/health"))["message"]
    names = sorted(l["name"] for l in health["supervised"])
    print(names)
    for want in ["graph", "effects", "build-dispatch", "eval-dispatch", "trigger-dispatch",
                 "cache-maintenance", "sign-sweep", "debug-index",
                 "eval-cache-sweep", "retention", "rollup", "outbound-connect"]:
        assert want in names, f"{want} missing from supervised loops: {names}"
    bad = [l for l in health["supervised"] if l["restarts"] or l["pass_timeouts"]]
    assert not bad, f"restarted or stalled loops: {bad}"
    assert health["workers_connected"] >= 1, health

    t0 = time.time()
    server.succeed("systemctl stop gradient-server.service")
    stop_secs = time.time() - t0
    print(f"gradient-server stopped in {stop_secs:.1f}s")
    assert stop_secs < 40, f"shutdown took {stop_secs:.1f}s; the drain budget is 30s"
    server.succeed("journalctl -u gradient-server --no-pager | grep -q 'background tasks drained cleanly'")

    banner("Phase 12: the worker survives the server stop and reconnects")
    builder.succeed("systemctl is-active gradient-worker.service")
    own_session = requires("Phase 12's closed-session checks", "distinct-upstream-workers")
    if own_session:
        builder.succeed("journalctl -u gradient-worker --no-pager | grep -q 'connection closed; reconnecting'")

    server.succeed("systemctl start gradient-server.service")
    server.wait_for_open_port(3000)
    if own_session:
        builder.wait_until_succeeds(
            "journalctl -u gradient-worker --no-pager | grep -q 'reconnected successfully'", timeout=180
        )

    banner("Phase 12b: SIGTERM stops the worker")
    t0 = time.time()
    builder.succeed("systemctl stop gradient-worker.service")
    stop_secs = time.time() - t0
    print(f"gradient-worker stopped in {stop_secs:.1f}s")
    assert stop_secs < 40, f"an idle worker took {stop_secs:.1f}s to stop"
    builder.succeed(
        "journalctl -u gradient-worker --no-pager | grep -q 'stop requested; aborting running jobs'"
    )
    assert builder.succeed(
        "systemctl show -p Result --value gradient-worker.service"
    ).strip() == "success"

    banner("Phase 14: pending deliveries survive a restart (#597)")
    action_id = sql("SELECT id FROM task_action WHERE name = 'restart-probe';")
    assert len(action_id) == 36, f"the state-declared probe action is missing: {action_id}"

    server.succeed("cat > /root/hook.py <<'PY'\n"
                   "from http.server import BaseHTTPRequestHandler, HTTPServer\n"
                   "class H(BaseHTTPRequestHandler):\n"
                   "    def do_POST(self):\n"
                   "        n = int(self.headers.get('Content-Length', 0))\n"
                   "        open('/root/hook.log', 'ab').write(self.rfile.read(n) + b'\\n')\n"
                   "        self.send_response(200); self.end_headers()\n"
                   "HTTPServer(('127.0.0.1', 8099), H).serve_forever()\n"
                   "PY")
    server.succeed("systemd-run --unit hook-probe ${pkgs.python3}/bin/python3 /root/hook.py")

    server.succeed("systemctl stop gradient-server.service")
    sql(
        "INSERT INTO pending_delivery (id, kind, key, payload, created_at, next_attempt_at) VALUES ("
        "gen_random_uuid(), 3, 'restart-probe', "
        f"""'{{"action": "{action_id}", "event": "evaluation.approval_granted", "envelope": {{"event": "evaluation.approval_granted", "at": "2026-01-01T00:00:00Z", "content": {{"probe": "restart"}}}}}}'::jsonb, """
        "now() AT TIME ZONE 'UTC', now() AT TIME ZONE 'UTC');"
    )
    server.succeed("systemctl start gradient-server.service")
    server.wait_for_open_port(3000)

    server.wait_until_succeeds("grep -q restart /root/hook.log", timeout=120)
    assert sql("SELECT delivered_at IS NOT NULL FROM pending_delivery WHERE key = 'restart-probe';") == "t", \
        "the row must settle, not be redelivered on every tick"

    banner("Phase 13: the SQL plan gate")
    for star in ("projects/project", "tasks/project/task", "caches/main"):
        server.succeed(
            f"{CURL} -sf -X PUT -H 'Authorization: Bearer {token}' "
            f"{API}/user/stars/{star}"
        )
    for node, unit in fleet_units():
        node.succeed(f"systemctl stop {unit}.service")
    server.succeed("systemctl stop gradient-server.service")

    print(server.succeed(
        "${pkgs.gradient.gate}/bin/gradient-sql-gate "
        "--database-url postgresql://postgres@127.0.0.1/gradient "
        "--max-unmeasured 40 2>/dev/console"
    ))

    banner("E2E test PASSED")
    '';
})
