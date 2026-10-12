# Connection

Workers talk to the server over a single WebSocket at `/proto`, with binary frames from the `Proto` derive. Sessions pass through **version agreement -> handshake -> authorization -> capabilities -> job loop**.

```mermaid
sequenceDiagram
      participant W as Worker
      participant S as Server
      W->>S: version range
      S->>W: version range
      W->>S: InitConnection { capabilities, id }
      S->>W: AuthChallenge { peers }
      W->>S: AuthResponse { tokens }
      S->>W: InitAck { authorized_peers, failed_peers }
      W->>S: WorkerCapabilities
      loop job loop
          W->>S: RequestJob
          S->>W: AssignJob
      end
```

## Handshake

| Step | Message | Content |
|---|---|---|
| 1 | `InitConnection` | `capabilities`, the worker's persistent `id` |
| 2 | `AuthChallenge` | The peers that registered this worker ID |
| 3 | `AuthResponse` | A token for each challenged peer |
| 4 | `InitAck` or `Reject` | Authorized peers, failed peers with reasons |

Worker IDs live in `/var/lib/gradient-worker/worker-id` on the worker. The server can match reconnects by that ID, reject duplicates and label the worker in the UI and logs.

The cap of a handshake message is `MAX_HANDSHAKE_MESSAGE_SIZE` (64 KiB). A larger message can end the session with close code `1002`, without decoding.

**Rejections:** The server can reject a session with the codes below. The connected worker can reject the server with `400`, `401`, `403` or `503`, see [Server-Initiated Handshake](#server-initiated-handshake).

| Code | Reason |
|---|---|
| `400` | An unexpected message during the handshake |
| `401` | `no valid peer tokens provided` |
| `403` | `unknown worker`, `worker is deactivated`, `no project grants this team's workers`, or the server is not accepting connections |
| `495` | `project has no cache subscribed`: every authorized project is lacking a cache |
| `496` | `worker already connected` |

## Server-Initiated Handshake

The server can connect to every registration and team worker holding a `url`. The server's tokens and the worker's TLS certificate prove each side.

```mermaid
sequenceDiagram
      participant S as Server
      participant W as Worker
      S->>W: Authenticate { worker_id, tokens }
      W->>S: InitConnection { capabilities, id }
      S->>W: InitAck { authorized_peers, failed_peers }
```

| Step | Message | Content |
|---|---|---|
| 1 | `Authenticate` | The worker ID, a `(peer, token)` per registration holding a stored token at the URL |
| 2 | `InitConnection` or `Reject` | `401` for an unknown worker ID or any wrong token, `400` for any other first message |
| 3 | `InitAck` or `Reject` | The projects whose tokens the worker accepted, without a challenge round |

A refusing side can answer with these codes.

| Code | Reason |
|---|---|
| `400` | A first message other than `InitConnection` |
| `401` | An unknown worker ID or any wrong token |
| `403` | `the link is disabled`, `the upstream is not allowed to run` |
| `503` | `the upstream is unreadable right now`, `the allowance is not checked yet` |

- The server can send its tokens over `wss://` URLs.
- A `ws://` URL is for local and operator setups only. The server can still send the tokens and log a warning.
- Workers check server tokens against the hashes in `services.gradient.worker.acceptedServerTokensFile` on the worker. Workers without the file accept every server.
- Reauths of a server-connected session get `AuthUpdate`, never `AuthChallenge`.
- Reauths finding a new registration close the session. The next connection can send the new project's token.
- Projects granting a team's workers join the running session. All of them share the team worker's single token for the team ID.
- A registration cannot reuse the worker ID of a team worker. Gradient.CI worker IDs belong to a single registration or team worker only.
- The server can keep the last failure of every worker ID in memory as the worker's offline reason.
- The server can keep a failure before the token check only for a registered worker ID.

## Capabilities

A capability is active when both sides support it. Only the server can set `core` and `cache`.

| Capability | Meaning |
|---|---|
| `core` | The server side of the protocol. Always on for the server, off for workers |
| `cache` | Serving as a binary cache. Always on for the server |
| `fetch` | Cloning repositories and prefetching flake inputs |
| `eval` | Running flake evaluations |
| `build` | Running Nix builds |
| `federate` | Reserved. Negotiated in the handshake, no behavior yet |

Capability flags guard new features, not version numbers.

## Authorization

A **peer** is anything registering a worker, such as a project, a cache or a proxy. Both sides consent. Peers register the worker ID and workers hold the peer's token.

1. A peer registered the worker ID. The server generated a token for that pair.
2. The peers file (`services.gradient.worker.peersFile`) is a list of `peer_id:token` lines.
3. The server can challenge every registering peer on connect and check each token on its own.

| Peer | Granted Access |
|---|---|
| Project | Jobs from the project's tasks |
| Cache | Serving and pulling from the cache |
| Proxy | Membership in the proxy's worker pool |

- The remaining peers keep the session open after some failed tokens.
- A failure of every token can reject the session.
- A project without a cache subscription can move to `failed_peers` in the reply.
- The reply is `495` when no peer remained.
- **Reauth** can add peers without reconnecting.
- The server can send `AuthChallenge` to a connected worker when a peer registered the worker.
- A cache subscription by a project is a reason for a reauth of the project's workers.
- The worker can send `ReauthRequest` when its peers file changed.
- Both end in `AuthUpdate` and reauth can never revoke granted peers.
- A single connection per worker ID. The server can reject a second connection with `496`.
- The server can drop a connection silent for `proto.workerHeartbeatTimeoutSecs` (120 s).
- The worker can drop a connection with data unacknowledged for 120 s and reconnect.
- The worker can abandon a connect after 10 s and a handshake after 15 s without an answer.
- A project's worker list can mark a worker as live when that worker authenticated for the project.

## Build Timeouts

The `timeout_secs` and `max_silent_secs` of a `BuildSpec` follow the derivation's own `timeout` and `maxSilent`. The fallbacks are `build.defaultTimeoutSecs` (14400) and `build.defaultMaxSilentSecs` (3600). A limit is off at a value of `0`. A build past a limit is a `FailedTimeout`. Evaluations have no timeout.

## Server Restart

A restart can lose work in flight, but no queued job.

| Side | Behavior |
|---|---|
| Worker | Aborting every running job and reconnecting with backoff (1 s, doubling up to 60 s) |
| Worker | Sending a full handshake, then `RequestJobList` and a `RequestJob` per kind |
| Server | Dropping every report whose `job_id` and `assignment_id` the current session did not assign |
| Server | `recover_interrupted_work` is closing open assignments and aborting running attempts before the first session. Every `Building` build and every interrupted evaluation is queued again |

## Server Stop

- Server sessions all close at once on `SIGTERM`.
- Workers abort every running job of the closed session.
- The next start can re-queue the interrupted work through [Startup recovery](../scheduler/waiting-and-recovery.md#startup-recovery).
- Workers reconnect once the server is back.
- A `Draining` message from the server can end the session, never the worker process.

## Version Agreement

```text
"GRAD" | oldest: u16 LE | newest: u16 LE
```

- Both sides send this 8-byte frame before the first message. Future versions leave it alone.
- The `ProtoSocket` type can agree on the first send or receive. Both sides are sending first and reading second.
- The agreed version is the lower of both newest versions. Every later frame is for that version.
- Two ranges without overlap can end the session with close code `1002` and `no shared protocol version: ours 30..=30, peer 31..=32`.
- A first frame without `GRAD` came from protocol 26 or older. The session can end with `peer protocol is older than 30`.
- The oldest supported version is 30. Older workers need an upgrade.
- Both sides are logging the reason at the `warn` level.
- A server connecting to a worker can keep the reason as the worker's offline reason.
- `gradient_wire::PROTO_VERSIONS` is the supported range, computed from the `#[proto]` annotations.
- The agreed version for feature checks is available through the `writer.version()` call.

## Changing a Message

| Change | Annotation | Effect |
|---|---|---|
| New field | `#[proto(N)]` | Field present since N. The oldest supported version is rising to N |
| New field with a fallback | `#[proto(N, default)]` | Older peers are leaving the field out. Decoding is filling `Default::default()` |
| New variant | `#[proto(N)]` on the variant | Appended at the end. Sending the variant to a peer below N is an `EncodeError` |
| Removed or reordered field or variant, changed meaning | `#[proto(oldest = N)]` on `ClientMessage` and `ServerMessage` | The oldest supported version is rising to N |

- A field without `default` is mandatory. A forgotten `default` can cost compatibility, never correctness.
- Variants are append-only. A variant older than its predecessor is a compile error.
- The files `gradient-wire/schema/v{N}.txt` hold the wire shape of each supported version. The `schema` test can compare the files to the code.
- `cargo run --example wire_schema` can write the file of a new version and delete files below the oldest. Existing files stay untouched.

## Cache Sessions

The `/cache/{cache}/proto` endpoint can offer the same frames as a read-only session for a single cache.

| Aspect | Rule |
|---|---|
| Public cache | No credentials, unless `proto.anonymousCache.enable` is off |
| Private cache | `Authorization: GRAD<key>` on the upgrade request |
| Handshake | `InitConnection`, then `InitAck` with no peers. No challenge |
| Allowed | `CacheQuery` in `Normal` or `Pull` mode, `NarRequest`. The server is rejecting everything else with `403` |
| Limits | `proto.anonymousCache.maxConnectionsPerIp` (32) anonymous connections per IP. Every session is counting against `proto.maxConnections` (256). `SAFE_INFLIGHT_MESSAGE_SIZE` (2 MiB) is capping each message, enough for a `CacheQuery` of 1000 paths |
| Idle | Closed after 120 s without a NAR transfer |

## Implementation

- A single handshake state machine in `gradient-wire/src/session/handshake.rs` is the driver of every session.
- The server can run `as_authority` for an incoming worker and `as_dialer` for a worker the server connected to. The worker can run `as_peer` and `as_dialed`.
- The cache session can agree on a version like every other session.
- The code in `session/frame.rs` can split each socket into a typed reader and a writer. The writer can hold a control lane and a bulk lane.
- Control frames go first. A bulk batch can hold no more than 256 KiB.
- A full bulk queue can never block control replies.
- Chunk payloads (`NarPush`, `UploadChunk`, `EvalCacheChunk`, `LogChunk`) arrive at the handler as slices of the frame, without a copy.
- The `client::dial` function can disable Nagle's algorithm on every socket. Small control frames go out without waiting for a delayed ACK.
