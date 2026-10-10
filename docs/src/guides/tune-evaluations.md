# Tune Evaluation Speed

An evaluator pool sized to the worker's cores and memory, and the attribute holding an evaluation back found and excluded.

**Requirements:**

- A worker with a few finished evaluations, see [Add a Remote Worker](remote-worker.md)
- The worker host's core count and RAM

## 1. Read the Pool's Numbers

Workers hold a pool of evaluation subprocesses. Attribute batches of the evaluated set go to the pool in parallel, and the slowest batch can decide the whole evaluation time.

Two worker log lines describe an evaluation.

```
discovery split into shard batches calls=118 pool=32
closure walk complete walked=879 elapsed_secs=1407
```

`calls` is the number of attribute batches, `pool` the number of subprocesses. About 4 batches per subprocess are formed, so 32 subprocesses and 500 attributes make about 125 batches of 4.

**Job Board -> Evals** can show peak memory per evaluation. The value is the largest evaluation subprocess seen during the evaluation and is the input for step 3.

## 2. Pick the Pool Size

`eval.forkWorkers` is the number of subprocesses. `null` means the core count, capped at 16. Two bounds apply.

- Cores: at the core count or below. Nix evaluation is single-threaded per subprocess, plus a few garbage collector threads.
- Memory: `forkWorkers * maxRss` below the host's RAM minus the memory for builds.

| Host | `forkWorkers` | `maxRss` | Pool memory |
|---|---|---|---|
| 8 cores, 32 GB | 8 | 2 GiB | 16 GiB |
| 16 cores, 64 GB | 16 | 3 GiB | 48 GiB |
| 64 cores, 128 GB | 32 | 4 GiB | 128 GiB worst case, about 60 GiB in practice |

Idle subprocesses hold their last heap, measured at 1.5 to 2 GB after NixOS system evaluations. The worst case in the table is reached only when all subprocesses sit at the cap.

```nix
services.gradient.worker.eval.forkWorkers = 32;
```

## 3. Set the Memory Cap

`eval.maxRss` is the resident memory above which idle subprocesses restart cold after their call. The cap is not a hard limit. A call may grow past it and finish.

Take the peak memory of the task's evaluations from step 1 and add a margin of about a third. NixOS system evaluations sit near 2 GB, the module default. A cap below the typical heap costs a cold start per batch, with the whole flake and nixpkgs evaluated again each time.

```nix
services.gradient.worker.eval.maxRss = 4 * 1024 * 1024 * 1024;
```

Cold restarts on each batch are visible in the daemon log. The number of distinct `accepted connection from pid` entries during an evaluation is then far above `calls`.

## 4. Guard the Host

`system.minFreeRamMb` is the free memory below which the worker can kill the largest evaluation subprocess and fail that evaluation. `0` means 10% of RAM, clamped between 128 MiB and 1 GiB. 32 subprocesses can grow past the cap together. 1 GiB is a thin margin for that.

```nix
services.gradient.worker.system.minFreeRamMb = 16384;
```

`eval.maxConcurrent` (default 1) is the number of evaluations per worker. Concurrent evaluations share the pool. A higher value can spread the same subprocesses over more evaluations.

## 5. Find a Serializing Attribute

An evaluation with idle subprocesses and a long `elapsed_secs` has 1 attribute keeping 1 subprocess busy. The loop below can time each attribute of the evaluated set with a cap and print those over the cap. Run it on a machine with the repository.

```sh
nix eval --json .#<set> --apply builtins.attrNames | tr -d '[]"' | tr ',' '\n' > attrs.txt # (1)!
xargs -P 8 -I{} bash -c 's=$(date +%s); timeout 25 nix eval --raw ".#<set>.{}.drvPath" >/dev/null 2>&1; echo "$(( $(date +%s) - s ))s rc=$? {}"' < attrs.txt | sort -rn | head
```

1.  `<set>` is the attribute set of the task's wildcard, for example `packages.x86_64-linux` or `checks.x86_64-linux`.

An attribute with `rc=124` hit the cap. Re-run those alone with a higher cap and `--show-trace` to see what they evaluate. Three patterns cause most slow attributes.

- A report over all hosts or all packages in 1 process.
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
    services.gradient.state.tasks.web-app.wildcard = "checks.x86_64-linux.*, !checks.x86_64-linux.snapshots";
    ```

## Verify Deployment

Trigger an evaluation and compare its `eval_derivations` span on the job page with the earlier evaluations. The daemon log can confirm the end of cold restarts. **Job Board -> Evals** can rank the evaluation among the recent evaluations by time and peak memory.

## Next Steps

- [Configuration Reference](../reference/configuration.md)
- [Evaluation Wildcards](../reference/wildcards.md)
- [Monitor Gradient](monitoring.md)
- [Add a Remote Worker](remote-worker.md)
