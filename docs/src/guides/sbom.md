# Export a Software Bill of Materials

Software bills of materials (SBOMs) cover every package inside the outputs of a task. Gradient can export the SBOM as a CycloneDX 1.6 JSON file. Scanners and license audits use the file.

**Requirements:**

- An evaluation that completed on a [task](../concepts/projects-and-tasks.md)
- A [cache](../concepts/caches.md) with task outputs

## 1. Download the SBOM

=== "UI"

    Go to the task page. Click the three-dot menu in the evaluation panel. Select **SBOM**.

=== "API"

    ```sh
    curl -sOJ -H "Authorization: Bearer $TOKEN" \
      "https://gradient.example.com/api/v1/tasks/$PROJECT/$TASK/sbom"
    ```

- The task's latest completed evaluation produced this file.
- Public projects need no token.

## 2. Read the Contents

| Field | Content |
|---|---|
| `metadata.component` | The task as `<project>/<task>`, with the evaluated commit as version |
| `components` | Every store path in the runtime closure of the entry point outputs |
| `name`, `version`, `purl` | Package name and version from the derivation, e.g. `pkg:nix/hello@2.12.1` |
| `hashes` | The SHA-256 NAR hash |
| `licenses` | The `meta.license` of an entry point, as an SPDX id where Nix knows one |
| `properties` | `nix:store_path`, `nix:nar_size` and the `nix:attribute` of an entry point |
| `dependencies` | Runtime references between the store paths |

- Compilers and other build-time dependencies never make it into the file.
- Only entry points get a `meta.license` lookup from Gradient. Dependencies arrive with no license.
- Packages with their own entry point carry a license in the SBOM.

!!! note "Incomplete SBOM"
    Gradient knows the references of a path only after the path reached the cache. A path missing from the cache appears without hashes and dependencies. The `compositions` entry then says `incomplete` and the property `gradient:complete` is `false`.

## Verify Deployment

Downloads carry the name `<project>-<task>-sbom.cdx.json`. The CycloneDX CLI can validate that file against the schema.

```sh
nix shell nixpkgs#cyclonedx-cli -c cyclonedx validate --input-file acme-demo-sbom.cdx.json
```

## Next steps

- [Caches](../concepts/caches.md)
- [API reference](../reference/api.md)
- [Projects and Tasks](../concepts/projects-and-tasks.md)
