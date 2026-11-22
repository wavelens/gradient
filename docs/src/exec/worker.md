<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

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
    subnet.v4 = "10.233.0.0/16";
    preWarm = 2; # (2)!
    maxPreWarm = 8; # (3)!
    labels = [ "lab-a" ];
    devices = [ # (4)!
      { name = "gpu:h100"; vfio = [ "0000:17:00.0" "0000:18:00.0" ]; projects = "all"; }
      { name = "camera"; usb = "1d6b:0002"; projects = "all"; }
    ];
    mounts = [ # (5)!
      { name = "datasets"; path = "/srv/datasets"; projects = "all"; readOnly = true; }
      { name = "scratch"; path = "/srv/scratch"; projects = "all"; perJob = true; }
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
4.  Set exactly `vfio`, `dev` or `usb` per device. Bind each VFIO device to `vfio-pci` first. Each device entry can take `projects`, a string `"all"`, or a list of project UUIDs. Jobs for projects that don't appear in the list count as missing the device class.
5.  Guests find a mount at `/mnt/<name>`. `perJob` mounts give every job a fresh directory below `path`. Mounts take `projects`, same as devices. Mount names consist of 1 to 64 ASCII letters, digits, `-`, or `_`. The names `work`, `control`, and `console` belong to the worker. Names outside these rules make the system evaluation fail.
6.  Workers send `allow` and `defaults` to the server. The server can apply both at placement.

!!! warning
    Polkit rules live in the module file. The worker user can start and stop `gradient-guest@<id>` units and nothing else. Fixed commands and settings for these units come from the module. A root supervisor for each unit can create taps, bind USB devices and run virtiofsd inside a namespace sandbox. These actions apply only to configured devices, mounts and bridges. cloud-hypervisor can run inside `gradient-guest-vm@<id>` as the worker user without capabilities.

## 2. Connect the Network

- Workers with exec jobs need ``exec.subnet.v4``, an IPv4 subnet for bridge ``exec.bridge``. Choose a subnet that no other network on the host can use. Networked jobs join the bridge with addresses from ``exec.subnet.v4``. NAT rules on the host connect the bridge to the outside.
- Worker modules set the guest bridge MAC address to `06:47:52:00:00:01`. That MAC address prevents the kernel from flushing the neighbour entries of running guests when guests join or leave the bridge. Administrators who need another value can set one through `networking.interfaces.<bridge>.macAddress`.
- Guests resolve names through a DNS forwarder on the bridge. Addresses in `exec.nameservers` replace the forwarder.

### Guest IPv6 { #guest-ipv6 }

- Guests on the network get one IPv6 address each from ``exec.subnet.v6``, in addition to their IPv4 address. The host offset of the IPv6 address in ``exec.subnet.v6`` is the same as the host offset of the IPv4 address in ``exec.subnet.v4``. The first host address is the gateway address of the bridge.
- Guests get a static IPv6 address and a default route via the gateway. Guests have no link-local address and ignore router advertisements. Guests reach the internet through IPv6 masquerade on the host.
- Use ``exec.allow.blockedGlobalIPv6Networks`` to list global IPv6 networks that guests cannot reach. Include the LAN prefix of the host in this list.
- With `exec.subnet.v6` set, this option has no default and needs a value. Write `[ ]` to block nothing. A missing value stops the NixOS evaluation with the message below. Without `exec.subnet.v6` the option has no effect.

```nix
# configuration.nix
services.gradient.worker.exec = {
  subnet.v4 = "10.233.0.0/16";
  subnet.v6 = "fd00:ec2:6::/64"; # (1)!
  allow.blockedGlobalIPv6Networks = [ "2001:db8:1234::/48" ]; # (2)!
};
```

1. A random, unique local `/64` from `fd00::/8`.
2. The global IPv6 prefix of the host LAN.

- `exec.subnet.v6` and `exec.allow.network` enable IPv6 forwarding and masquerading for the whole host through `networking.nat.enableIPv6`. All interfaces also get `accept_ra = 2` from the same option. Hosts with static IPv6 addresses now accept router advertisements. They ignored them before. Hosts that get addresses through kernel SLAAC keep a default route only with value 2.
- Rebuild the guest image for IPv6. Rebuilt guest images use guest protocol 11. Old images still work with IPv4 only. They have no IPv6 at all. The bridge filter can drop their IPv6 frames. The worker can write the line below to the job log once per job.

```text
The guest image is too old for IPv6. The protocol of the guest agent in this image is <version>. The first protocol with IPv6 support is 11. IPv4 is the only option for this job. Rebuild the guest image to provide IPv6 for the job.
```

## 3. Limit the Guest Network { #guest-network }

- Networked guests reach the internet through NAT on the host.
- Filter rules drop traffic from guests on the worker to other guests. The same rules drop traffic to private, shared, link-local, loopback, multicast, and reserved IPv4 ranges. The same rules drop traffic to all ports on the host. The endpoint `169.254.169.254` for cloud metadata is part of the link-local range. Filter rules drop this address unless `exec.allow.allowedPrivateNetworks` includes that address.
- A guest can respond to connections that arrive from outside. One example is a connection from another node in a multi-node job. Filter rules allow the replies to those connections, even when the destination is a private address. Only replies travel this path. A guest can't send its own traffic to a dropped range, even over connections that opened while the guest tables were missing.
- DNS forwarding is on when ``exec.nameservers`` is empty. Filter rules allow DNS queries to the bridge address in that case.
- Filter rules drop VLAN frames from guests. A guest can't ping the bridge address over IPv4 or IPv6.
- Filter rules drop every IPv6 frame from guests without `exec.subnet.v6`.
- A guest on IPv6 can reach global unicast addresses in `2000::/3` only. Filter rules also drop networks in `2001::/23`, `2001:db8::/32`, `2002::/16`, `3fff::/20` and `exec.allow.blockedGlobalIPv6Networks`.
- Guests send DNS queries over IPv4 only.
- Guests send traffic only with a MAC address, IPv4 address, and IPv6 address from the lease of the guest.

Two options reopen a single target.

| Option | Default | Effect |
|---|---|---|
| `exec.allow.allowedPrivateNetworks` | `[ ]` | IPv4 and IPv6 networks in CIDR form that guests can still reach. IPv6 entries apply only with `exec.subnet.v6`. Guests on one worker never reach each other, even with their own subnet in the list |
| `exec.allow.guestConnectableHostTcpPorts` | `[ ]` | TCP ports of the worker host that guests can reach. The module also opens them in the host firewall for `exec.bridge`. TCP only |

```nix
# configuration.nix
services.gradient.worker.exec.allow = {
  allowedPrivateNetworks = [ "10.234.0.0/16" "192.168.10.5/32" ]; # (1)!
  guestConnectableHostTcpPorts = [ 3000 ]; # (2)!
};
```

1. Guest subnets of other workers in a multi-node job, and the package mirror on the LAN.
2. The Gradient server on the worker host (for jobs that call its API).

### Filter Tables

- All exec workers load tables `bridge gradient-guests` and `inet gradient-guests` through unit `gradient-guest-nftables`.
- The unit doesn't touch other tables. Guest tables work the same with an iptables firewall and with `networking.nftables.enable`.
- Entries of set `guest-tap-addresses` in `bridge gradient-guests` map each guest on the network to an interface, a MAC address and an IPv4 address. The guest supervisor adds an entry at guest start and removes it at guest end.
- Entries of set `guest-tap-ipv6-addresses` store the interface, MAC address and IPv6 address for each guest with IPv6. Records for `/run/gradient-guest-tap-addresses` always have three lines. Line 1 is the IPv4 entry. Line 2 is the IPv6 entry or a blank line. Line 3 is the guest ID.
- Forward rules and input rules sit in `inet gradient-guests`.
- New network jobs fail while table `inet gradient-guests` doesn't exist. Job errors include that table name.
- Records in `/run/gradient-guest-tap-addresses` come from the guest supervisor, one per network-connected guest. Start and reload events restore the records of running guests to the set.
- Forcibly terminated guests leave nothing behind. The `ExecStopPost` step of `gradient-guest@<id>` removes the tap of that guest, the entries for that tap in both sets and the IPv6 neighbour entries of that tap on stop, also after SIGKILL.
- Both tables stay in place after a unit stop. Running guests keep their filters. Guest starts launch the unit again.
- The worker module is responsible for the addition of `ExecStartPost` and `ExecStopPost` steps to `nftables.service` with guest networking enabled and `networking.nftables.enable` set. These steps reload the guest tables and the entries for running guests right after start and stop of that unit, in that order. Each step can run the `gradient-guest-nftables` load script for up to 10 seconds. Failures go to the journal and don't fail start or stop.

| Event | Effect on the guest tables |
|---|---|
| `nixos-rebuild switch` without a host firewall change, `systemctl reload gradient-guest-nftables` | Rules change in one transaction. Running guests keep their network and filter. |
| Reload, restart or stop of `nftables.service` with `flushRuleset = false` | Guest tables stay unchanged. Running guests keep network and filters |
| Reload of `nftables.service` with `flushRuleset = true` | Flush operations delete guest tables. The immediate next step is a reload of `gradient-guest-nftables` by systemd. Tables and entries for running guests come back within a few milliseconds. Running guests keep network connectivity, but no filter is active in between. |
| Restart of `nftables.service` with `flushRuleset = true` | A flush of the guest tables and a load by the stop step of the worker NixOS module (`ExecStopPost`) occur in that order during stop. The start flush and the start-step load (`ExecStartPost`) follow in the same order. A systemd restart of `gradient-guest-nftables` and a load of the same tables by the unit then occur with no gap. Live guests stay with no filter twice, a few milliseconds each time |
| Stop of `nftables.service` with `flushRuleset = true` | Flush operations delete guest tables. The next step is a stop of `gradient-guest-nftables` from systemd. A new step in `nftables.service` is part of the worker module. Tables and entries of running guests reload right after a flush. Running guests have no filter for a few milliseconds. |
| Start of `nftables.service` with `flushRuleset = true` | Guest tables flush on every start. The start step of the worker module can reload the tables and entries of running guests right after the host rules. Running guests stay without a filter for a few milliseconds. |
| `systemctl stop gradient-guest-nftables` | Rules and entries stay in the tables. Flushes with the unit stopped leave running guests without a filter until the next start of the unit. A manual start, a start or restart of `nftables.service`, or the next start of a guest with network access all start the unit again |
| Reload of the iptables `firewall.service` | No effect |
| Manual `nft flush ruleset` | Guest tables disappeared. Running network guests keep the network without filters. New network jobs fail until `systemctl reload gradient-guest-nftables` |

Run `systemctl reload gradient-guest-nftables` right after a manual flush. The reload does restore both the table and the entries for running guests.

The root user on a worker host can remove the guest filter with `nft flush ruleset`, by deleting the guest tables by hand, or with a flush after `gradient-guest-nftables` stopped. Running guests keep network access without a filter until the tables are back. The module doesn't stop the guests.

Turning exec off does not remove the guest table. Delete both with `nft delete table bridge gradient-guests` and `nft delete table inet gradient-guests` after the switch.

## 4. Pick the Restart Behaviour

| `exec.stopGuestsWithWorker` | Worker restart |
|---|---|
| `true` (default) | All guests stop. Running jobs restart in a new attempt without using a retry |
| `false` | Guests keep going. The restarted worker can reattach and resume the log of the same attempt |

## 5. Size the Guest Budget { #guest-size }

Workers provide a budget for guest CPUs, memory and disk. Both running guests and warm guests draw from that budget.

| Worker option | Default |
|---|---|
| `exec.guestCpus` | CPU count of the host |
| `exec.guestMemoryMb` | 80 % of the host memory |
| `exec.guestDiskMb` | 80 % of the free space under `<worker.baseDir>/guests` at worker start |

- Only workers with enough free budget start jobs. Jobs with no such worker stay queued, the same way jobs wait on busy devices.
- Sizes above the budget of all workers appear in the wait reason (example: `needs 64 CPUs`).
- Jobs that no connected worker can handle fail after 300 seconds. Set task option `wait_for_workers` to keep them queued instead.
- `exec.preWarm` guests boot with `gradient.guest.cpus` and `gradient.guest.memoryMb` of the image, `2` and `500` by default.
- Queued jobs of other sizes get warm guests of their own size too. `exec.maxPreWarm` can cap the warm guests of all sizes together.
- Jobs take a warm guest only with exactly its CPU count and memory. Other sizes boot a new guest.
- Matching jobs go to a worker with a warm guest first. Other workers start such a job after about 10 seconds.
- `/work` is a disk of `diskMb` with the checkout. Checkouts larger than the disk fail the job with `the checkout does not fit in diskMb = <n>`.
- Guests boot with a writable `/nix/store` disk of `exec.guestStoreMb`, 20480 by default. Workers copy the program closure onto that disk.
- Writable store disks are sparse and take only the space the guest writes. They stay outside of `exec.guestDiskMb`.

## Verify Deployment

- The worker shows the `exec` capability on its registration, see [Workers](../concepts/workers.md).
- Exec jobs with matching requests leave the queue and start on the worker.

## Next Steps

- [Declare an Exec App](apps.md): requests matched against the worker
- [Workers](../concepts/workers.md): registrations, capabilities and worker sops files
- [Configuration](../reference/configuration.md): every `worker.exec` option
