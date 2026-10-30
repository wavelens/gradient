# Gradient Exec

Flake apps started on workers after each evaluation, with the hardware they ask for. Every exec job boots in a fresh virtual machine with a checkout of the evaluated commit.

```mermaid
flowchart LR
    app["App with passthru.gradient"] --> build["Program build"]
    build --> job["Exec job"]
    job --> worker["Exec worker"]
    worker --> results["Phases, outputs, pull requests"]
```

<div class="grid cards" markdown>

-   :material-toggle-switch: **[Turn On Exec Jobs](enable.md)**

    The four switches between a repository and a worker.

-   :material-server: **[Set Up an Exec Worker](worker.md)**

    Guest image, devices, mounts and limits of a worker.

-   :material-application-cog: **[Declare an Exec App](apps.md)**

    `passthru.gradient`, hardware requests and the job context.

-   :material-key: **[Pass Secrets](secrets.md)**

    Project secrets, repository sops files and worker sops files.

-   :material-console: **[Open a Terminal](terminals.md)**

    Browser terminals, SSH logins and port forwards into a job.

-   :material-disc: **[Boot a Custom Image](custom-images.md)**

    A `nixosConfiguration` as the guest system of one app.

-   :material-lan: **[Spread a Job over Workers](multi-node.md)**

    One guest per node, on workers of one zone.

-   :material-flag-checkered: **[Report Phases and Upload Outputs](phases-and-outputs.md)**

    Restart points and files from inside a job.

-   :material-source-pull: **[Open Pull Requests](pull-requests.md)**

    The job token, `gradient pr open` and the `exec_tree` action.

-   :material-link-variant: **[Chain Evaluations](chain-evaluations.md)**

    New evaluations and entry points after a finished job.

</div>
