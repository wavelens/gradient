# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

# Each capture is a transient unit stopped with SIGINT, which tcpdump, perf and
# strace all answer by flushing their output and exiting.

BENCH = "/tmp/bench"
TRACE_DIRS = {"server": "/var/lib/gradient/trace", "worker": "/var/lib/gradient-worker/trace"}


def start_capture(node, unit, command, output):
    node.succeed(
        f"systemd-run --unit={unit} --collect --property=KillSignal=SIGINT -- {command}"
    )
    node.wait_until_succeeds(f"test -e {output}", timeout=30)


def stop_capture(node, unit):
    node.execute(f"systemctl stop {unit}")


def run_dir(node, run):
    return f"{BENCH}/{run}" if node is server else f"{BENCH}/worker"


def prepare(run):
    server.succeed(f"rm -rf {BENCH}/{run} && mkdir -p {BENCH}/{run}/trace {BENCH}/{run}/pg")
    worker.succeed(f"rm -rf {BENCH}/worker && mkdir -p {BENCH}/worker/trace {BENCH}/worker/strace")
    server.succeed(f"truncate -s 0 {TRACE_DIRS['server']}/*.jsonl || true")
    worker.succeed(f"truncate -s 0 {TRACE_DIRS['worker']}/*.jsonl || true")


def start_pcaps(run):
    out = run_dir(server, run)
    start_capture(server, "cap-proto",
                  f"tcpdump -i any -U -w {out}/proto.pcap 'tcp port 80 and not host 127.0.0.1'",
                  f"{out}/proto.pcap")
    start_capture(server, "cap-pg", f"tcpdump -i lo -U -w {out}/pg.pcap 'tcp port 5432'",
                  f"{out}/pg.pcap")


def stop_pcaps():
    stop_capture(server, "cap-proto")
    stop_capture(server, "cap-pg")


def start_profilers(run):
    for node in (server, worker):
        out = run_dir(node, run)
        start_capture(node, "cap-perf",
                      f"perf record -a -g -F 499 -e cpu-clock -o {out}/perf.data",
                      f"{out}/perf.data")

    pid = worker.succeed("systemctl show -p MainPID --value gradient-worker").strip()
    evals = worker.succeed("pgrep -f '[-]-eval-subprocess' || true").split()
    attach = " ".join(f"-p {p}" for p in [pid, *evals])
    out = run_dir(worker, run)
    start_capture(worker, "cap-strace",
                  f"strace -f -ff -tt -T -o {out}/strace/worker {attach}",
                  f"{out}/strace/worker.{pid}")


def stop_profilers(run):
    stop_capture(worker, "cap-strace")
    for node in (server, worker):
        stop_capture(node, "cap-perf")
        out = run_dir(node, run)
        node.succeed(
            f"perf script -i {out}/perf.data 2>/dev/null | stackcollapse-perf.pl"
            f" | flamegraph.pl --title '{node.name} {run}' > {out}/flame.svg"
        )


def set_auto_explain(on):
    duration, analyze = ("0", "on") if on else ("-1", "off")
    psql(f"ALTER SYSTEM SET auto_explain.log_min_duration = {duration};"
         f" ALTER SYSTEM SET auto_explain.log_analyze = {analyze};"
         " SELECT pg_reload_conf();", database="postgres")


def collect_traces(run):
    for node in (server, worker):
        name = "server" if node is server else "worker"
        node.succeed(f"cp {TRACE_DIRS[name]}/*.jsonl {run_dir(node, run)}/trace/")


def copy_out(run):
    server.copy_from_machine(f"{BENCH}/{run}")
    worker.copy_from_machine(f"{BENCH}/worker", run)
    host = pathlib.Path(server.out_dir) / run
    for spans in (host / "worker" / "trace").glob("*.jsonl"):
        spans.rename(host / "trace" / spans.name)
    return host
