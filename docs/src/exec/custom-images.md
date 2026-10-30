# Boot a Custom Image

One exec app on its own NixOS guest system, in place of the worker image. Kernel modules, drivers and system services of the job go into the custom image.

**Requirements:**

- An exec app, see [Declare an Exec App](apps.md)
- An exec worker allowing custom images, see [Set Up an Exec Worker](worker.md)

## 1. Allow Custom Images

```nix
# configuration.nix of the worker
services.gradient.worker.exec.allow.customImages = true;
```

Jobs with `image` wait for a worker with [`exec.allow.customImages`](../reference/configuration.md#workerexec). That setting defaults to `false`.

## 2. Define the Image

```nix
# flake.nix
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.gradient.url = "github:wavelens/gradient";

  outputs = { self, nixpkgs, gradient }: let
    pkgs = nixpkgs.legacyPackages.x86_64-linux;
  in {
    nixosConfigurations.trainer = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        gradient.nixosModules.guest # (1)!
        ({ config, ... }: {
          gradient.guest.enable = true;
          nixpkgs.config.allowUnfree = true;
          hardware.graphics.enable = true;
          hardware.nvidia.open = true;
          services.xserver.videoDrivers = [ "nvidia" ];
          system.stateVersion = "26.05";
        })
      ];
    };

    apps.x86_64-linux.train = {
      type = "app";
      program = "${pkgs.writeShellScript "train" "nvidia-smi && python train.py"}";
      passthru.gradient = {
        image = self.nixosConfigurations.trainer;
        devices = [ "gpu:h100:8" ];
      };
    };
  };
}
```

1.  Guest agents come from the `gradient.guest` module. Images that lack that module fail with `guest image has no gradient agent, import the gradient.guest module`.

## 3. Keep the Image in Step

- Workers build the image the same way they build the program.
- Jobs with an image run on a fresh guest, booted cold from the image.
- Warm guests stay untouched.
- `cpus` and `memoryMb` come from the worker image. The custom image settings don't count.
- Guest agents in the image come from the `gradient` input. Keep that input at the server version.
- Agents from older releases start the program in `/work`. Those agents can refuse shells.

## Verify Deployment

- Jobs reach `running` on a worker with `exec.allow.customImages`.
- Custom image boot failures fail the attempt and consume a retry.

## Next Steps

- [Set Up an Exec Worker](worker.md): the worker image and `exec.allow`
- [Declare an Exec App](apps.md): every `passthru.gradient` field
- [Configuration](../reference/configuration.md): `exec.allow.customImages`
