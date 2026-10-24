# Export a Software Bill of Materials

All packages in build outputs appear in a software bill of materials (SBOM). Gradient can export the SBOM as a CycloneDX 1.6 JSON file. Scanners use the file. License audits use the file.

**Requirements:**

- A build with the status `Completed` or `Substituted`
- A [cache](../concepts/caches.md) with the build outputs

## 1. Download the SBOM

=== "Task Page"

    Go to the task page. Find a package. Open its three-dot menu. Pick **SBOM**.

=== "Log Page"

    Builds on the log page have three-dot menus. Open one. Select **SBOM**.

=== "API"

    ```sh
    curl -sOJ -H "Authorization: Bearer $TOKEN" \
      "https://gradient.example.com/api/v1/builds/$BUILD_ID/sbom"
    ```

- The menu entry stays grey until the build has outputs.
- Public projects need no token.

## 2. Read the Contents

| Field | Content |
|---|---|
| `metadata.component` | The derivation name of the build, with the evaluated commit as version |
| `components` | Every store path in the runtime closure of the build outputs |
| `name`, `version`, `purl` | Package name and version from the derivation, e.g. `pkg:nix/hello@2.12.3` |
| `hashes` | The SHA-256 NAR hash |
| `licenses` | The `meta.license` of an entry point, as an SPDX id where Nix knows one |
| `properties` | `nix:store_path`, `nix:nar_size` and the `nix:attribute` of an entry point |
| `dependencies` | Runtime references between the store paths |

- Compilers don't land in the file. Neither do other build-time dependencies.
- Only entry points get a `meta.license` from Gradient. Dependencies show up with no license.

!!! note "Incomplete SBOM"
    Gradient knows the references of a path only after the path reached the cache. A path missing from the cache appears without hashes and dependencies. The `compositions` entry then says `incomplete` and the property `gradient:complete` is `false`.

## Verify Deployment

Browsers save `<derivation name>-sbom.cdx.json`. The CycloneDX CLI can validate that file against the schema.

```sh
nix shell nixpkgs#cyclonedx-cli -c cyclonedx validate --input-file hello-2.12.3-sbom.cdx.json
```

## Next steps

- [Caches](../concepts/caches.md)
- [API reference](../reference/api.md)
- [Projects and Tasks](../concepts/projects-and-tasks.md)
