# Pass Secrets

All logs mask secret values in exec jobs.

**Requirements:**

- An exec app, see [Declare an Exec App](apps.md)
- The sops files in the repository require [sops](https://github.com/getsops/sops) 3.10 or later.

## 1. List the Secrets

Jobs list secret names in `secrets`. Each name needs exactly one source.

```nix
# flake.nix
passthru.gradient = {
  secrets = [ "wandb-token" "hf-token" "lab-key" ];
  sops = [ ./secrets/train.yaml ]; # (1)!
  args = [ "--token" "@gradient.secret.hf-token@" ];
};
```

1.  Repository files encrypted with sops. Evaluations hand on the ciphertext only.

| Source | Setup | Use |
|---|---|---|
| Project secrets | Project **Settings -> Secrets**, the API or `secret_files` in the state | Values from project admins, the simplest choice |
| Repository sops files | Files listed in `sops`, encrypted for the project recipient | Values versioned with the code, never typed into Gradient |
| Worker sops files | `exec.sops.files` on the worker, see [Workers](../concepts/workers.md#exec-secrets) | Operator values out of reach of the server |

## 2. Set Project Secrets

=== "UI"

Open project **Settings -> Secrets** and click **Set secret**. Values remain write-only. Only project admins can change secrets.

=== "Declarative"

    ```nix
    services.gradient.state.projects.acme.secret_files = {
      wandb-token = "/run/secrets/wandb-token";
    };
    ```

- A script can set values with `PUT /projects/{project}/secrets/{name}`. See [API](../reference/api.md) for details.
- A name can contain 1-64 lowercase letters, digits, or dashes. The maximum size of a value is 64 KiB.
- Secrets from state show as managed. UI and API changes to those secrets fail.

## 3. Encrypt Repository Sops Files

Project **Settings -> Secrets** can display the **Sops recipient** derived from the SSH key of the project.

```yaml
# .sops.yaml
creation_rules:
  - path_regex: secrets/.*\.(yaml|json)$
    age: age1... # (1)!
```

1. The recipient from the **Secrets** page.

```sh
sops encrypt --in-place secrets/train.yaml
```

- Top-level string values turn into secrets. Files need YAML or JSON, picked by extension.
- RSA project keys have no `age1` recipient. The SSH public key works as the age recipient with sops 3.10 or newer.
- Files with encrypted comments fail. Remove the comments before encrypting.

## 4. Read Secrets in the Job

| Form | Example |
|---|---|
| File | `/run/gradient/secrets/wandb-token` |
| Variable | `GRADIENT_SECRET_WANDB_TOKEN` |
| Placeholder | `@gradient.secret.wandb-token@` in `args` and `env` |

- Logs, terminals and the console show `[secret:<name>]` in place of a value. Base64 and URL-encoded values also receive masks.
- Values shorter than 8 bytes stay unmasked. Binary outputs and port forwards stay unmasked too.

!!! warning "Swap on the Worker Host"
    Guest memory holds the secrets of a job. The host can move guest memory to swap. Set [`worker.exec.noSwap`](../reference/configuration.md#workerexec) to keep guests in RAM, or give the host encrypted swap.

## Verify Deployment

Jobs with a secret problem fail before the start, without using a retry.

| Message | Cause |
|---|---|
| `secret "<name>" found in two sources: <first>, <second>` | Two sources hold the name |
| `secret "<name>" not found in project secrets, repository sops files or worker sops files` | No source holds the name |
| `sops file <file>: not encrypted for the project key` | A recipient other than the project key |

## Next Steps

- [Workers](../concepts/workers.md#exec-secrets): worker sops files
- [Declarative State](../reference/state.md): `secret_files`
- [Open a Terminal](terminals.md): masked terminal sessions
