/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, lib, daemon ? pkgs.gradient-daemon-mock or null }:
let
  inherit (lib) mapAttrs foldl' concatMap attrNames attrValues elem all recursiveUpdate splitString;

  defaultTiming = {
    seed = 1;
    scale = 1.0;
    buildMs = { dist = "lognormal"; median = 40; p99 = 800; };
    chunkMs = { dist = "uniform"; min = 0; max = 3; };
    chunkBytes = 65536;
  };

  defaultNode = id: {
    name = id;
    deps = [ ];
    requiredSystemFeatures = [ ];
    preferLocalBuild = false;
    allowSubstitutes = true;
    fixedOutput = false;
    outputs.out = { };
    build = {
      durationMs = null;
      outcome = "success";
      failStatus = "PermanentFailure";
      log = [ "building ${id}" ];
    };
    present = { workers = [ ]; cache = false; };
  };

  defaultOutput = { size = 4096; references = [ ]; products = [ ]; };

  fail = msg: throw "store-spec: ${msg}";

  refNode = r: builtins.head (splitString "." r);

  closure = spec: id:
    let go = seen: i: if elem i seen then seen else foldl' go (seen ++ [ i ]) spec.derivations.${i}.deps;
    in go [ ] id;

  references = node: concatMap (o: o.references) (attrValues node.outputs);

  checkNode = spec: id: node:
    let
      ids = attrNames spec.derivations;
      buildClosure = closure spec id;
      depsKnown = all (d: elem d ids || fail "${id}: unknown dep ${d}") node.deps;
      refsInClosure = all (r: elem (refNode r) buildClosure || fail "${id}: reference ${r} is outside its build closure") (references node);
      fodShape = !node.fixedOutput
        || (attrNames node.outputs == [ "out" ] && node.outputs.out.references == [ ] && node.outputs.out.products == [ ])
        || fail "${id}: a FOD has one output `out` without references or products";
      workersClosed = all (r: all (w: elem w spec.derivations.${refNode r}.present.workers) node.present.workers
        || fail "${id}: present.workers not closed under ${r}") (references node);
      cacheClosed = all (r: !node.present.cache || spec.derivations.${refNode r}.present.cache
        || fail "${id}: present.cache not closed under ${r}") (references node);
    in
    depsKnown && refsInClosure && fodShape && workersClosed && cacheClosed;

  validate = spec:
    let
      ids = attrNames spec.derivations;
      nodesValid = all (id: checkNode spec id spec.derivations.${id}) ids;
      acyclic = all (id: !(elem id (concatMap (closure spec) spec.derivations.${id}.deps)) || fail "cycle through ${id}") ids;
      entriesKnown = all (e: elem e ids || fail "unknown entry point ${e}") spec.entryPoints;
    in
    assert nodesValid && acyclic && entriesKnown; spec;

  withDefaults = raw: raw // {
    system = raw.system or "x86_64-linux";
    timing = recursiveUpdate defaultTiming (raw.timing or { });
    entryPoints = raw.entryPoints or (attrNames raw.derivations);
    derivations = mapAttrs
      (id: node:
        let merged = recursiveUpdate (defaultNode id) node;
        in merged // { outputs = mapAttrs (_: o: defaultOutput // o) merged.outputs; })
      raw.derivations;
  };

  normalize = raw: if raw ? name then validate (withDefaults raw) else fail "every spec needs a name";

  resolve = raw:
    let
      spec = normalize raw;
      built = import ./derivations.nix spec;
      outPath = id: o: builtins.unsafeDiscardStringContext built.drvs.${id}.${o}.outPath;
      refPath = r: let parts = splitString "." r; in outPath (builtins.elemAt parts 0) (builtins.elemAt parts 1);
      resolveNode = id: node: node // {
        drvPath = builtins.unsafeDiscardStringContext built.drvs.${id}.drvPath;
        fodContent = if node.fixedOutput then built.fodContent id else null;
        inherit (spec) timing;
        outputs = mapAttrs (o: out: out // { path = outPath id o; references = map refPath out.references; }) node.outputs;
      };
    in
    spec // { derivations = mapAttrs resolveNode spec.derivations; };

  toFlake = raw:
    let spec = normalize raw;
    in pkgs.runCommand "store-spec-flake-${spec.name}" { } ''
      mkdir -p $out
      cp ${./derivations.nix} $out/derivations.nix
      echo '{"nodes":{"root":{}},"root":"root","version":7}' > $out/flake.lock
      cat > $out/store-spec.nix <<'EOF'
      ${lib.generators.toPretty { } spec}
      EOF
      cat > $out/flake.nix <<'EOF'
      {
        outputs = _:
          let
            spec = import ./store-spec.nix;
            built = import ./derivations.nix spec;
          in
          {
            packages.''${spec.system} = builtins.listToAttrs (map (e: { name = e; value = built.drvs.''${e}; }) spec.entryPoints);
          };
      }
      EOF
    '';

  daemonNode = spec: id: node: lib.nameValuePair "${spec.name}/${id}"
    (removeAttrs node [ "deps" "requiredSystemFeatures" "preferLocalBuild" "allowSubstitutes" ]);

  toDaemonConfig = raws: worker: pkgs.writeText "gradient-daemon-${worker}.json" (builtins.toJSON {
    inherit worker;
    timing = defaultTiming;
    derivations = foldl' (acc: spec: acc // lib.mapAttrs' (daemonNode spec) spec.derivations) { } (map resolve raws);
  });

  toUpstreamCache = raws: pkgs.runCommand "store-spec-upstream" { } ''
    ${daemon}/bin/gradient-daemon mock cache-export \
      --spec ${toDaemonConfig raws "upstream"} \
      --secret-key-file ${./keys/upstream.sec} \
      --out $out
  '';
in
{
  inherit normalize resolve toFlake toDaemonConfig toUpstreamCache;
  upstreamPublicKey = lib.fileContents ./keys/upstream.pub;
  presets = import ./presets.nix;
}
