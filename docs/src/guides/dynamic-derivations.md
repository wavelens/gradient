# Use Dynamic Derivations

A flake with derivations generated at build time, built and cached by Gradient.

**Requirements:**

- A [task](../concepts/projects-and-tasks.md) on a flake
- Workers with the `eval` and `build` [capabilities](../concepts/workers.md#capabilities)

## 1. Enable the Nix Features

The worker module came with `ca-derivations` already on. Enable `dynamic-derivations` on every worker host.

```nix
# configuration.nix
nix.settings.experimental-features = [ "dynamic-derivations" ]; # (1)!
```

1. Worker module features can merge with this list in NixOS.

- Worker hosts keep their features in `nix.conf`. Evaluators read that file.
- The `nix-daemon` can reject a generated derivation without the feature.

## 2. Write a Dynamic Derivation

Dynamic derivations need a `.drv` file. Another derivation made that file as output.

```nix
# flake.nix
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }: let
    pkgs = nixpkgs.legacyPackages.x86_64-linux;
    contentAddressed = mode: { # (1)!
      __contentAddressed = true;
      outputHashMode = mode;
      outputHashAlgo = "sha256";
    };

    hello = pkgs.runCommand "hello" (contentAddressed "nar") ''
      mkdir $out
      echo "Hello from a generated derivation" > $out/greeting
    '';

    helloDrv = pkgs.runCommand "hello.drv" (contentAddressed "text") '' # (2)!
      cp ${builtins.unsafeDiscardOutputDependency hello.drvPath} $out
    '';
  in {
    packages.x86_64-linux.default = pkgs.runCommand "dynamic-hello" (contentAddressed "nar") ''
      cp -r ${builtins.outputOf helloDrv.outPath "out"} $out # (3)!
    '';
  };
}
```

1. All derivations in the chain carry content-addressed outputs.
2. Text-hashed outputs hold the `.drv` file, and their names end in `.drv`.
3. `builtins.outputOf` can point at the `out` output of the generated derivation.

## 3. Allow Recursive Nix

Flakes without nested `nix` calls don't need this step. A build with recursive Nix can call `nix` inside the sandbox, for example `nix derivation add`.

```nix
# configuration.nix
nix.settings = {
  experimental-features = [ "recursive-nix" ];
  system-features = [ "nixos-test" "benchmark" "big-parallel" "kvm" "recursive-nix" ]; # (1)!
};
```

1.  A list here can replace the NixOS default. The first four entries keep the default features.

- Derivations that use recursive Nix list `requiredSystemFeatures = [ "recursive-nix" ]`.
- Workers report the host's `system-features`. Gradient can place such a build only on a worker with `recursive-nix`.
- Nested `nix` calls produce paths. Those paths land in the cache next to the outputs that refer to them.

## Verify Deployment

Push the flake and open the task page of the new evaluation.

- Task pages show `other.x86_64-linux.hello.drv` with an **IFD** tag. The evaluating worker waited on that build. Then the worker read the generated `.drv`.
- Dependencies of `packages.x86_64-linux.default` include the generated `hello`.
- A recursive Nix build without a matching worker can wait with the reason `x86_64-linux with features recursive-nix`.

Generating builds behave like [Imported Derivations](../concepts/evaluations-and-builds.md#imported-derivations). Failed generating builds fail the dependent attributes with `resolving a dynamic derivation input failed: ...`.

## Limits

- Dynamic derivations over [SSH](build-over-ssh.md) end with the error `dynamic derivations are not supported over SSH`.
- Floating content-addressed derivations get no support over [SSH](build-over-ssh.md) either.

## Next steps

- [Evaluations and Builds](../concepts/evaluations-and-builds.md)
- [Workers](../concepts/workers.md)
- [Add a Remote Worker](remote-worker.md)
- [Tune Evaluation Speed](tune-evaluations.md)
