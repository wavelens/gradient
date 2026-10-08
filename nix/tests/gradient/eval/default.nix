/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }: {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-eval";
    globalTimeout = 1800;

    nodes.machine = { pkgs, lib, ... }: {
      networking.firewall.enable = false;
      documentation.enable = false;
      virtualisation = {
        cores = 2;
        memorySize = 2048;
        writableStore = true;
      };

      nix.settings = {
        experimental-features = [ "nix-command" "flakes" ];
        substituters = lib.mkForce [ ];
        max-jobs = 0;
      };

      environment.systemPackages = with pkgs; [ git ];
    };

    testScript = { nodes, ... }:
      let
        worker = "${lib.getExe' pkgs.gradient "gradient-worker"}";
        git = "${lib.getExe pkgs.git}";
        nix = "${lib.getExe pkgs.nix}";
      in
      ''
      import base64
      import json

      REPO = "git+file:///root/fixture"

      def banner(msg):
          print(f"\n=== {msg} ===")

      start_all()
      machine.wait_for_unit("multi-user.target")

      banner("Stage fixture flake")
      machine.succeed("install -Dm644 ${./fixture.nix} /root/fixture/flake.nix")
      machine.succeed("${git} -C /root/fixture init -q")
      machine.succeed("${git} -C /root/fixture config user.email t@t && ${git} -C /root/fixture config user.name t")
      machine.succeed("${git} -C /root/fixture add flake.nix")
      machine.succeed("${git} -C /root/fixture commit -qm fixture")
      machine.succeed("${nix} flake lock /root/fixture")
      machine.succeed("${git} -C /root/fixture add -A && ${git} -C /root/fixture commit -qm lock --allow-empty")

      # The hidden `--eval-driver` harness is reading JSON-line requests.
      # It is passing them through the real binary transport to a real subprocess.
      # Shutdown is producing no response line.
      banner("Run eval-worker via --eval-driver")
      requests = [
          {"op": "list", "repository": REPO, "wildcards": ["packages.x86_64-linux.*"]},
          {"op": "list", "repository": REPO, "wildcards": ["packages.x86_64-linux.#"]},
          {"op": "list", "repository": REPO,
           "wildcards": ["packages.x86_64-linux.*", "!packages.x86_64-linux.cowsay"]},
          {"op": "fingerprint", "repository": REPO},
          {"op": "shutdown"},
      ]
      DRIVER = (
          "HOME=/root GRADIENT_WORKER_SERVER_URL=ws://dummy/proto "
          "GRADIENT_WORKER_EVAL_CACHE_DIR=/root/eval-cache "
          "${worker} --eval-driver"
      )

      def drive(tag):
          payload = "".join(json.dumps(r) + "\n" for r in requests)
          b64 = base64.b64encode(payload.encode()).decode()
          machine.succeed(f"echo {b64} | base64 -d > /root/reqs-{tag}.jsonl")

          status, _ = machine.execute(
              f"start=$(date +%s%3N); {DRIVER} /root/reqs-{tag}.jsonl"
              f" > /root/out-{tag}.jsonl 2> /root/eval-{tag}.log; rc=$?;"
              f" echo $(( $(date +%s%3N) - start )) > /root/ms-{tag}; exit $rc"
          )
          print(machine.succeed(f"cat /root/out-{tag}.jsonl || true"))
          print(machine.succeed(f"cat /root/eval-{tag}.log || true"))
          assert status == 0, f"eval driver ({tag}) exited {status}; see the log above"

          out = machine.succeed(f"cat /root/out-{tag}.jsonl").splitlines()
          responses = [json.loads(l) for l in out if l.strip()]
          assert len(responses) == 4, f"expected 4 responses, got {len(responses)}: {responses}"
          return responses, int(machine.succeed(f"cat /root/ms-{tag}").strip())

      responses, cold_ms = drive("cold")

      banner("Assert wildcard parity")
      hello = "packages.x86_64-linux.hello"
      cowsay = "packages.x86_64-linux.cowsay"

      nested = [{"pattern": "packages.x86_64-linux.nested.#", "only": ["inner"]}]

      assert responses[0]["kind"] == "list_ok", responses[0]
      star = {it["attr"] for it in responses[0]["items"]}
      assert star == {hello, cowsay}, f"trailing-* mismatch: {star}"
      assert responses[0]["deferred"] == nested, f"nested set not deferred: {responses[0]}"

      hash_ = {it["attr"] for it in responses[1]["items"]}
      assert hash_ == {hello, cowsay}, f"# should be non-recursive: {hash_}"

      excluded = {it["attr"] for it in responses[2]["items"]}
      assert excluded == {hello}, f"exclusion mismatch: {excluded}"
      assert responses[2]["deferred"] == nested, f"nested set not deferred: {responses[2]}"

      banner("Assert resolved paths + per-attr isolation")
      items = {it["attr"]: it for it in responses[0]["items"]}

      h = items[hello]
      assert h.get("error") is None and h.get("drv_path", "").endswith(".drv"), h

      boom_errors = [e for e in responses[0]["errors"] if e["attr"] == "packages.x86_64-linux.boom"]
      assert boom_errors and boom_errors[0]["message"], f"boom must isolate as a per-attr error: {responses[0]['errors']}"

      banner("Assert fingerprint matches the eval-cache filename")
      assert responses[3]["kind"] == "fingerprint_ok", responses[3]
      fp = responses[3].get("fingerprint")
      assert fp, f"expected a fingerprint for the committed flake, got {fp}"
      machine.succeed(f"test -f /root/eval-cache/eval-cache-v6/{fp}.sqlite")

      # A re-eval paying the fixture's seconds of evaluation again has read nothing back (#657).
      # The fingerprint must match too. A different key would be a cold blob the timing cannot reveal.
      banner("Assert the second eval starts warm off the eval cache")
      warm, warm_ms = drive("warm")
      assert warm[3].get("fingerprint") == fp, f"fingerprint moved: {fp} -> {warm[3].get('fingerprint')}"

      warm_items = {it["attr"]: it for it in warm[0]["items"]}
      assert warm_items[hello].get("drv_path") == h["drv_path"], f"warm resolve disagrees: {warm_items[hello]}"

      print(f"cold={cold_ms}ms warm={warm_ms}ms")
      assert warm_ms * 2 < cold_ms, f"second eval was not warm: cold={cold_ms}ms warm={warm_ms}ms"

      banner("Eval test PASSED")
      '';
  });
}
