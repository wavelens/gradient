# Tune Evaluation Speed

Two factors control evaluation speed. First, the evaluator pool has to fit the cores and memory of the worker. Second, every attribute that adds evaluation overhead has to be found and excluded.

**Requirements:**

- A worker with several finished evaluations, see [Add a Remote Worker](remote-worker.md)
- Core count and RAM of the worker host

## 1. Read the Pool's Numbers

Every worker owns a pool of evaluation subprocesses. The pool runs attribute batches of the evaluated set in parallel. The slowest batch can decide the total evaluation time.

Two log lines describe an evaluation:

```
discovery split into shard batches calls=118 pool=32
closure walk complete walked=879 elapsed_secs=1407
```

`calls` is the number of attribute batches. `pool` is the number of subprocesses. The split targets about 4 batches per subprocess. Example: 32 subprocesses and 500 attributes give roughly 125 batches of 4.

**Job Board -> Evals** can show peak memory per evaluation. This value is the largest evaluation subprocess seen during the run. Step 3 uses it as input.

## 2. Pick the Pool Size

`eval.forkWorkers` controls how many subprocesses run in parallel. Set it to `null` to use the core count, capped at 16. Two limits constrain the value.

- Cores: keep the pool at or below the core count. Each subprocess evaluates on a single thread, with a few extra garbage collector threads.
- Memory: keep `forkWorkers * maxRss` below the host's RAM minus memory reserved for builds.

| Host | `forkWorkers` | `maxRss` | Pool memory |
|---|---|---|---|
| 8 cores, 32 GB | 8 | 2 GiB | 16 GiB |
| 16 cores, 64 GB | 16 | 3 GiB | 48 GiB |
| 64 cores, 128 GB | 32 | 4 GiB | 128 GiB worst case, about 60 GiB in practice |

Idle subprocesses hold their last heap in memory. NixOS system evaluations measured this at 1.5 to 2 GB per subprocess. The table's worst case applies only when every subprocess sits at the heap cap.

```nix
services.gradient.worker.eval.forkWorkers = 32;
```

## 3. Set the Memory Cap

The option `eval.maxRss` holds a resident memory threshold for worker subprocesses. Idle subprocesses that grow past this threshold restart cold after their next call. The threshold does not force termination. A subprocess can go over the cap and still finish its current call.

Take the peak memory of the task's evaluations from step 1 and add a margin of about a third. NixOS system evaluations sit near 2 GB, the module default. A cap below the typical heap costs a cold start per batch, with the whole flake and nixpkgs evaluated again each time.

```nix
services.gradient.worker.eval.maxRss = 4 * 1024 * 1024 * 1024;
```

Cold restarts on each batch are visible in the daemon log. The number of distinct `accepted connection from pid` entries during an evaluation is then far above `calls`.

## 4. Guard the Host

`system.minFreeRamMb` define a memory floor for the host. Free RAM below this value allow the worker to kill the largest evaluation subprocess and mark that evaluation as failed. A value of `0` mean 10% of total RAM, clamped to a range of 128 MiB to 1 GiB. With up to 32 subprocesses that can grow past the cap at the same time, 1 GiB leave a thin margin.

```nix
services.gradient.worker.system.minFreeRamMb = 16384;
```

`eval.maxConcurrent` control how many evaluations run per worker, with a default of 1. Concurrent evaluations share one memory pool, so a higher value can spread the same subprocesses across more evaluations.

## 5. Find a Serializing Attribute

A subprocess that sits idle with a large `elapsed_secs` value hit one attribute in the evaluated set that blocks the rest. Time each attribute with a cap and print those over it. Run the loop below on a machine with the repository checked out.

```sh
nix eval --json .#<set> --apply builtins.attrNames | tr -d '[]"' | tr ',' '\n' > attrs.txt # (1)!
xargs -P 8 -I{} bash -c 's=$(date +%s); timeout 25 nix eval --raw ".#<set>.{}.drvPath" >/dev/null 2>&1; echo "$(( $(date +%s) - s ))s rc=$? {}"' < attrs.txt | sort -rn | head
```

1.  `<set>` names the attribute set behind the task's wildcard, for example `packages.x86_64-linux` or `checks.x86_64-linux`.

An attribute with `rc=124` hit the cap. Re-run those alone with a higher cap and `--show-trace` to see what they evaluate. Three patterns cause most slow attributes.

- One process produces a report across all hosts or all packages.
- `lib.generators.toPretty` or `builtins.toJSON` over `pkgs` or a NixOS `config`.
- `builtins.readDir` over a large tree.

Exclude an unneeded attribute with an `!` pattern, see [Evaluation Wildcards](../reference/wildcards.md).

=== "UI"

    Open the task's settings and edit **Evaluation Wildcard**.

    ```
    checks.x86_64-linux.*, !checks.x86_64-linux.snapshots
    ```

=== "Declarative"

    ```nix
    services.gradient.state.tasks.web-app.wildcard = "checks.x86_64-linux.**, !checks.x86_64-linux.snapshots";
    ```

## Verify Deployment

Open the job page of a new evaluation and find its `eval_derivations` span. Compare that span with spans from earlier evaluations.

The daemon log contains a message once a cold restart has finished.

Recent evaluations appear under **Job Board -> Evals** together with their timestamp and peak memory. That list allows ranking of the new run against previous ones.

## Next Steps

- [Configuration Reference](../reference/configuration.md)
- [Evaluation Wildcards](../reference/wildcards.md)
- [Monitor Gradient](monitoring.md)
- [Add a Remote Worker](remote-worker.md)
- [Run Multi-Node Evaluations](multi-node-evaluations.md)
