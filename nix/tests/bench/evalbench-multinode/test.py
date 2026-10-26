# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

CPUS = sorted(os.sched_getaffinity(0))


def assert_enough_cpus():
    needed = len(WORKER_NODES) * WORKER_CORES + SERVER_CORES
    assert len(CPUS) >= needed, (
        f"the multinode evalbench needs {needed} CPUs for its VMs, this builder has {len(CPUS)}"
    )


def pin(machine, cpus):
    for thread in pathlib.Path(f"/proc/{machine.process.pid}/task").iterdir():
        os.sched_setaffinity(int(thread.name), cpus)


def pin_vms():
    pin(server, CPUS[:SERVER_CORES])
    for index, node in enumerate(WORKER_NODES):
        first = SERVER_CORES + WORKER_CORES * index
        pin(node, CPUS[first:first + WORKER_CORES])


def assert_shaped_links():
    for node in WORKER_NODES:
        reply = node.succeed("ping -c 3 server")
        average_ms = float(re.search(r"= [\d.]+/([\d.]+)/", reply).group(1))
        assert average_ms >= 2 * DELAY_MS, f"{node.name}: {average_ms} ms round trip to the server"


assert_enough_cpus()
start_all()
pin_vms()
banner("bring services up")
server.wait_for_unit("gradient-server.service")
server.wait_for_unit("nginx.service")
wait_workers_ready()
assert_shaped_links()
