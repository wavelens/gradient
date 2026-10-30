# Set Up an Exec Worker

A worker starting every exec job in a fresh cloud-hypervisor guest.

**Requirements:**

- A worker host with `/dev/kvm`, see [Add a Remote Worker](../guides/remote-worker.md)
- Exec jobs turned on for the instance, see [Turn On Exec Jobs](enable.md)

## 1. Configure the Worker

```nix
# configuration.nix
services.gradient.worker = {
  capabilities.exec = true;
  exec = {
    image = gradient.lib.mkWorkerImage { # (1)!
      modules = [ ./worker-guest.nix ];
    };
    preWarm = 2; # (2)!
    maxPreWarm = 8; # (3)!
    labels = [ "lab-a" ];
    devices = [ # (4)!
      { name = "gpu:h100"; vfio = [ "0000:17:00.0" "0000:18:00.0" ]; }
      { name = "camera"; usb = "1d6b:0002"; }
    ];
    mounts = [ # (5)!
      { name = "datasets"; path = "/srv/datasets"; readOnly = true; }
      { name = "scratch"; path = "/srv/scratch"; perJob = true; }
    ];
    allow = { # (6)!
      network = true;
      devices = [ "gpu:h100" "camera" ];
      mounts = [ "datasets" "scratch" ];
      retries = 2;
    };
    defaults.mounts = [ "scratch" ];
  };
};
```

1.  A NixOS system for the guests. `gradient.guest.preScript` and `gradient.guest.postScript` tune every guest. `gradient.guest.cpus` and `gradient.guest.memoryMb` size the warm guests, see [Guest Size](#guest-size). Workers without `image` boot a plain guest image.
2.  Guests booted and paused ahead, waiting for a job. `0` by default.
3.  Upper bound for warm guests for upcoming exec jobs on the server. `3` by default. Set `0` for a fixed `preWarm`.
4.  Set exactly `vfio`, `dev` or `usb` per device. Bind each VFIO device to `vfio-pci` first.
5.  Guests find a mount at `/mnt/<name>`. `perJob` mounts give every job a fresh directory below `path`.
6.  Workers send `allow` and `defaults` to the server. The server can apply both at placement.

!!! warning
    Polkit rules from the module let the worker user start and stop `gradient-guest-*` units. Those units run as root. The worker user is root-equivalent on the host.

## 2. Connect the Network

- Networked jobs join the bridge `exec.bridge` with an address from `exec.subnet`. NAT rules on the host connect the bridge to the outside.
- Guests resolve names through a DNS forwarder on the bridge. Addresses in `exec.nameservers` replace the forwarder.

## 3. Pick the Restart Behaviour

| `exec.stopGuestsWithWorker` | Worker restart |
|---|---|
| `true` (default) | All guests stop. Running jobs restart in a new attempt without using a retry |
| `false` | Guests keep going. The restarted worker can reattach and resume the log of the same attempt |

## 4. Size the Guest Budget { #guest-size }

Workers provide a budget for guest CPUs, memory and disk. Both running guests and warm guests draw from that budget.

| Worker option | Default |
|---|---|
| `exec.guestCpus` | CPU count of the host |
| `exec.guestMemoryMb` | 80 % of the host memory |
| `exec.guestDiskMb` | 80 % of the free space under `<worker.baseDir>/guests` at worker start |

- Only workers with enough free budget start jobs. Jobs with no such worker stay queued, the same way jobs wait on busy devices.
- Sizes beyond the budget of every worker show in the wait reason, for example `needs 64 CPUs`.
- `exec.preWarm` guests boot with `gradient.guest.cpus` and `gradient.guest.memoryMb` of the image, `2` and `500` by default.
- Queued jobs of other sizes get warm guests of their own size too. `exec.maxPreWarm` can cap the warm guests of all sizes together.
- Jobs take a warm guest only with exactly its CPU count and memory. Other sizes boot a new guest.
- Matching jobs go to a worker with a warm guest first. Other workers start such a job after about 10 seconds.
- `/work` is a disk of `diskMb` with the checkout. Checkouts larger than the disk fail the job with `the checkout does not fit in diskMb = <n>`.

## Verify Deployment

- The worker shows the `exec` capability on its registration, see [Workers](../concepts/workers.md).
- Exec jobs with matching requests leave the queue and start on the worker.

## Next Steps

- [Declare an Exec App](apps.md): requests matched against the worker
- [Workers](../concepts/workers.md): registrations, capabilities and worker sops files
- [Configuration](../reference/configuration.md): every `worker.exec` option
