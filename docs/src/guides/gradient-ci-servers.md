# Connect Gradient.CI Servers

Build capacity from servers.gradient.ci for one project or for the whole instance, connected with one pasted token.

**Requirements:**

- A project with a cache subscription, see [First Project](../get-started/first-project.md)
- An account on [servers.gradient.ci](https://servers.gradient.ci)
- `gradientCi.enable` left on, see [Turn the Offer Off](#turn-the-offer-off)

## 1. Connect a Project

1. Open **Settings -> Workers** in the project. The first entry is **Gradient.CI Servers**.
2. Select **Connect**, then **Open servers.gradient.ci** in the dialog.
3. Sign in, name the key and select **Create key**. The page is showing the connection token once.
4. Paste the token into **Connection token** and select **Connect**.

A connection token is starting with `gci1_`. The entry is turning **Connected** within 30 s.

## 2. Connect a Base Server

A superuser is connecting one [base worker](../concepts/workers.md#base-workers) for every project of the instance.

1. Open **Job Board -> System Health -> Base Workers**.
2. Select **Connect base server** and paste the token as in step 1.

The dialog is confirming the connection with a notice. Gradient is dialing the base server only after a project enables the base server.

## 3. Enable the Base Server

The base server is building for a project only after **Enable** on the project's **Settings -> Workers** page.

## Verify Deployment

- The **Gradient.CI Servers** entry is showing **Connected**.
- The next evaluation of the project is showing builds on **Gradient.CI Servers**.

## Offline Reasons

An offline entry is showing its last failure to members who manage workers.

| Reason | Fix |
|---|---|
| `dial failed: ...` or `dial timed out after 10 s` | Allow outbound HTTPS from the Gradient server to servers.gradient.ci. |
| `401 unknown worker id or wrong token` | A deleted key or a mistyped token. **Disconnect** and connect with a new key. |
| `handshake failed: no shared protocol version: ...` | Gradient and servers.gradient.ci share no protocol version. Update Gradient. |
| `base worker not enabled by any project` | **Enable** the base server in a project. |
| `no connection token stored; register the worker again` | A worker with a `url` registered before Gradient.CI Servers. Delete and register the worker again. |
| `495 project has no cache subscribed` | Subscribe the project to a cache. |
| `stored connection token cannot be decrypted with the current crypt key` | `secrets.cryptFile` changed. **Disconnect** and connect with a new key. |

## Disconnect

- **Disconnect** on a project entry is deleting the project's connection.
- **Disable** is removing the base server from one project.
- **Disconnect** on **Base Workers** is removing the base server from every project.
- The key on servers.gradient.ci is staying until a deletion on the servers.gradient.ci keys page.

## Turn the Offer Off

```nix
services.gradient.gradientCi.enable = false;
```

**Connect** and **Enable** disappear. Existing connections keep building and stay listed.

## Next Steps

- [Workers](../concepts/workers.md): capabilities, matching and access
- [`gradientCi` options](../reference/configuration.md#gradientci)
- [Add a Remote Worker](remote-worker.md): a machine of the instance's own
