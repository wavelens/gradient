# Connect Gradient.CI Servers

servers.gradient.ci sells build capacity. A single pasted token connects that capacity to the IDE. One connection can serve a single project or an entire team.

**Requirements:**

- A project with a cache subscription, see [First Project](../get-started/first-project.md)
- An account on [servers.gradient.ci](https://servers.gradient.ci)
- `gradientCi.enable` left on, see [Turn the Offer Off](#turn-the-offer-off)

## 1. Choose Project or Team

Connect one thing per connection: a project or a [team](../concepts/teams.md). Never both.

| Connection | Builds for | Fits |
|---|---|---|
| Project | One project | A single project |
| Team | Every project that grants the team's [workers](../concepts/teams.md#team-workers) | Many projects behind one key |

Run builds for more than one project? Pick a team connection. One key then handles every project, and each new project needs only a grant.

## 2. Connect

=== "Project"

    1. In the project, open **Settings -> Workers**. **Gradient.CI Servers** appear as the first entry.
    2. Select **Connect** and pick **Open servers.gradient.ci** in the dialog.
    3. Sign in on servers.gradient.ci, give the key a name and select **Create key**. The connection token appear once on the page.
    4. Copy the token into the **Connection token** field and select **Connect**.

    The entry switch to **Connected** within 30 s.

=== "Team"

    Only a team Admin can perform this connection.

    1. Open the team's **Workers** page and select **Connect Gradient.CI Servers**.
    2. Pick **Open servers.gradient.ci** in the dialog.
    3. Sign in, name the key and select **Create key**. The connection token appear once on the page.
    4. Copy the token into the **Connection token** field and select **Connect**.
    5. On the **Members & Roles** page of each project, grant the team **Workers** so builds can use the connection.

    A project start building on the connection only after this grant.

Connection tokens start with `gci1_`.

## Verify Deployment

- Check that **Gradient.CI Servers** shows the status **Connected**.
- Run a new evaluation of the project. Builds appear on **Gradient.CI Servers**.

## Allowed Capabilities

A new connection grants `eval` and `build`. The entry shows both as **Allowed**. Fetching of sources stays the job of the project's own workers.

| Workers of the project | Allowed on Gradient.CI Servers |
|---|---|
| An own worker allowed to fetch | `eval`, `build` (default) |
| Gradient.CI Servers only | `fetch`, `eval`, `build` |

- **Edit** on the entry changes the allowed [capabilities](../concepts/workers.md#capabilities).
- **Deactivate** pauses the connection without a deletion.
- Edit team connections on the team's **Workers** page.

## Offline Reasons

Workers that are offline display their most recent failure. Only members who manage workers can see this.

| Reason | Fix |
|---|---|
| `dial failed: ...` or `dial timed out after 10 s` | Open outbound HTTPS from the Gradient server to servers.gradient.ci. |
| `401 unknown worker id or wrong token` | The key was deleted or the token contains a typo. **Disconnect**, then connect again with a new key. |
| `handshake failed: no shared protocol version: ...` | Gradient and servers.gradient.ci have no protocol version in common. Update Gradient. |
| `no project grants this team's workers` | Grant the team's workers on a project. |
| `no connection token stored; register the worker again` | The worker has a `url` and was registered before Gradient.CI Servers existed. Delete the worker and register it again. |
| `495 project has no cache subscribed` | Subscribe the project to a cache. |
| `stored connection token cannot be decrypted with the current crypt key` | The value of `secrets.cryptFile` changed. **Disconnect**, then connect again with a new key. |

## Disconnect

- **Disconnect** on a project entry removes the project connection.
- **Stop Workers** on a team grant removes the team connection from that project.
- **Delete** on the team's **Workers** page removes the team connection from every project.
- The key on servers.gradient.ci remains in place until deleted on the servers.gradient.ci keys page.

## Turn the Offer Off

```nix
services.gradient.gradientCi.enable = false;
```

**Connect** and **Connect Gradient.CI Servers** disappear. Existing connections keep building and stay listed.

## Next Steps

- [Workers](../concepts/workers.md): capabilities, matching and access
- [`gradientCi` options](../reference/configuration.md#gradientci)
- [Add a Remote Worker](remote-worker.md): a machine of the instance's own
