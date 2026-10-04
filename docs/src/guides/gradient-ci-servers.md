# Connect Gradient.CI Servers

Build capacity from servers.gradient.ci, connected with one pasted token. One connection can build for a project or a whole team.

**Requirements:**

- A project with a cache subscription, see [First Project](../get-started/first-project.md)
- An account on [servers.gradient.ci](https://servers.gradient.ci)
- `gradientCi.enable` left on, see [Turn the Offer Off](#turn-the-offer-off)

## 1. Choose Project or Team

One connection is enough. Connect either a project or a [team](../concepts/teams.md), not both.

| Connection | Builds for | Fits |
|---|---|---|
| Project | One project | A single project |
| Team | Every project granting the team's [workers](../concepts/teams.md#team-workers) | Many projects behind one key |

A team connection is the better pick for more than one project. One key can build for every project, and each new project needs only a grant.

## 2. Connect

=== "Project"

    1. Open **Settings -> Workers** in the project. The first entry is **Gradient.CI Servers**.
    2. Select **Connect**, then **Open servers.gradient.ci** in the dialog.
    3. Sign in, name the key and select **Create key**. The page will show the connection token once.
    4. Paste the token into **Connection token** and select **Connect**.

    The entry will turn **Connected** within 30 s.

=== "Team"

    A team Admin can connect the team.

    1. Open the team's **Workers** page and select **Connect Gradient.CI Servers**.
    2. Select **Open servers.gradient.ci** in the dialog.
    3. Sign in, name the key and select **Create key**. The page will show the connection token once.
    4. Paste the token into **Connection token** and select **Connect**.
    5. Grant the team **Workers** on the **Members & Roles** page of every project to build on the connection.

    Builds of a project start only after the grant.

Connection tokens start with `gci1_`.

## Verify Deployment

- The **Gradient.CI Servers** entry will show **Connected**.
- The next evaluation of the project will show builds on **Gradient.CI Servers**.

## Offline Reasons

Offline entries show their last failure to members who manage workers.

| Reason | Fix |
|---|---|
| `dial failed: ...` or `dial timed out after 10 s` | Allow outbound HTTPS from the Gradient server to servers.gradient.ci. |
| `401 unknown worker id or wrong token` | A deleted key or a mistyped token. **Disconnect** and connect with a new key. |
| `handshake failed: no shared protocol version: ...` | Gradient and servers.gradient.ci share no protocol version. Update Gradient. |
| `no project grants this team's workers` | Grant the team's workers on a project. |
| `no connection token stored; register the worker again` | A worker with a `url` registered before Gradient.CI Servers. Delete and register the worker again. |
| `495 project has no cache subscribed` | Subscribe the project to a cache. |
| `stored connection token cannot be decrypted with the current crypt key` | `secrets.cryptFile` changed. **Disconnect** and connect with a new key. |

## Disconnect

- **Disconnect** on a project entry: project connection deleted.
- **Stop Workers** on a project's team grant: team connection removed from that project.
- **Delete** on the team's **Workers** page: team connection removed from every project.
- The key on servers.gradient.ci will stay until a deletion on the servers.gradient.ci keys page.

## Turn the Offer Off

```nix
services.gradient.gradientCi.enable = false;
```

**Connect** and **Connect Gradient.CI Servers** disappear. Existing connections keep building and stay listed.

## Next Steps

- [Workers](../concepts/workers.md): capabilities, matching and access
- [`gradientCi` options](../reference/configuration.md#gradientci)
- [Add a Remote Worker](remote-worker.md): a machine of the instance's own
