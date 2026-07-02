/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ self, pkgs, ... }: {
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

      # ── Stage the fixture as a clean, locked, committed git flake ──────────
      banner("Stage fixture flake")
      machine.succeed("install -Dm644 ${./fixture.nix} /root/fixture/flake.nix")
      machine.succeed("${git} -C /root/fixture init -q")
      machine.succeed("${git} -C /root/fixture config user.email t@t && ${git} -C /root/fixture config user.name t")
      machine.succeed("${git} -C /root/fixture add flake.nix")
      machine.succeed("${git} -C /root/fixture commit -qm fixture")
      machine.succeed("${nix} flake lock /root/fixture")
      machine.succeed("${git} -C /root/fixture add -A && ${git} -C /root/fixture commit -qm lock --allow-empty")

      # ── Drive the eval-worker over the production rkyv transport ───────────
      # The wire is binary frames, so the hidden `--eval-driver` harness reads
      # these requests as JSON lines, runs them through the real parent-side
      # transport (spawn, version handshake, frames, streamed resolve) against
      # a real subprocess, and prints one JSON response line per request.
      # Shutdown produces no response, so we expect one line per other request.
      banner("Run eval-worker via --eval-driver")
      requests = [
          {"op": "list", "repository": REPO, "wildcards": ["packages.x86_64-linux.*"]},
          {"op": "list", "repository": REPO, "wildcards": ["packages.x86_64-linux.#"]},
          {"op": "list", "repository": REPO,
           "wildcards": ["packages.x86_64-linux.*", "!packages.x86_64-linux.cowsay"]},
          {"op": "resolve", "repository": REPO,
           "attrs": ["packages.x86_64-linux.hello", "packages.x86_64-linux.boom"]},
          {"op": "fingerprint", "repository": REPO},
          {"op": "shutdown"},
      ]
      payload = "".join(json.dumps(r) + "\n" for r in requests)
      b64 = base64.b64encode(payload.encode()).decode()
      machine.succeed(f"echo {b64} | base64 -d > /root/reqs.jsonl")

      status, _ = machine.execute(
          "HOME=/root GRADIENT_WORKER_SERVER_URL=ws://dummy/proto "
          "GRADIENT_EVAL_CACHE_DIR=/root/eval-cache "
          "${worker} --eval-driver /root/reqs.jsonl > /root/out.jsonl 2> /root/eval.log"
      )
      print(machine.succeed("cat /root/out.jsonl || true"))
      print(machine.succeed("cat /root/eval.log || true"))
      assert status == 0, f"eval driver exited {status}; see eval.log above"

      responses = [json.loads(l) for l in machine.succeed("cat /root/out.jsonl").splitlines() if l.strip()]
      assert len(responses) == 5, f"expected 5 responses, got {len(responses)}: {responses}"

      # ── Wildcard parity ────────────────────────────────────────────────────
      banner("Assert wildcard parity")
      hello = "packages.x86_64-linux.hello"
      cowsay = "packages.x86_64-linux.cowsay"
      inner = "packages.x86_64-linux.nested.inner"

      assert responses[0]["kind"] == "list_ok", responses[0]
      star = set(responses[0]["attrs"])
      assert star == {hello, cowsay, inner}, f"trailing-* mismatch: {star}"

      hash_ = set(responses[1]["attrs"])
      assert hash_ == {hello, cowsay}, f"# should be non-recursive: {hash_}"

      excluded = set(responses[2]["attrs"])
      assert excluded == {hello, inner}, f"exclusion mismatch: {excluded}"

      # ── Resolve + per-attribute isolation (#139) ───────────────────────────
      banner("Assert resolve + per-attr isolation")
      assert responses[3]["kind"] == "resolve_ok", responses[3]
      items = {it["attr"]: it for it in responses[3]["items"]}

      # ResolvedItem omits None fields (serde skip_serializing_if), so a clean
      # resolve has no `error` key and a failed one has no `drv_path` key.
      h = items[hello]
      assert h.get("error") is None and h.get("drv_path", "").endswith(".drv"), h

      b = items["packages.x86_64-linux.boom"]
      assert b.get("drv_path") is None and b.get("error"), f"boom must isolate as a per-item error: {b}"

      # ── Fingerprint ↔ on-disk eval-cache path agreement (#386 L3) ──────────
      # The lock-only `fingerprint` op must yield the same key Nix names the
      # on-disk cache after, so the worker can stage/pull `<fp>.sqlite`. The
      # driver spawns the subprocess like the production pool, exporting the
      # configured eval-cache dir as NIX_CACHE_HOME.
      banner("Assert fingerprint matches the eval-cache filename")
      assert responses[4]["kind"] == "fingerprint_ok", responses[4]
      fp = responses[4].get("fingerprint")
      assert fp, f"expected a fingerprint for the committed flake, got {fp}"
      machine.succeed(f"test -f /root/eval-cache/eval-cache-v6/{fp}.sqlite")

      banner("Eval test PASSED")
      '';
  });
}
