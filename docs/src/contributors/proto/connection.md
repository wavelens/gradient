# Connection

Workers talk to the server over one WebSocket at `/proto`, with binary frames from the `Proto` derive. Sessions pass through **version agreement -> handshake -> authorization -> capabilities -> job loop**.

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
| 3 | `AuthResponse` | One token per challenged peer the worker is holding |
| 4 | `InitAck` or `Reject` | Authorized peers, failed peers with reasons |

The worker ID is stored in `/var/lib/gradient-worker/worker-id` on the worker. The server will use the ID to match reconnects, reject duplicates and label the worker in the UI and logs.

Every handshake message has a cap of `MAX_HANDSHAKE_MESSAGE_SIZE` (64 KiB). A larger message will end the session with close code `1002`, without decoding.

**Rejections:** The server can reject a session with the codes below. A dialed worker can reject the server with `400` or `401`, see [Server-Dialed Handshake](#server-dialed-handshake).

| Code | Reason |
|---|---|
| `400` | An unexpected message during the handshake |
| `401` | `no valid peer tokens provided` |
| `403` | `unknown worker`, `worker is deactivated`, `no project grants this team's workers`, or the server is not accepting connections |
| `495` | `project has no cache subscribed`: every authorized project is lacking a cache |
| `496` | `worker already connected` |

## Server-Dialed Handshake

The server can dial every registration and team worker with a `url` set. The server will prove itself with its tokens, and the worker's TLS certificate will prove the worker.

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
| 1 | `Authenticate` | The dialed `worker_id`, one `(peer, token)` per registration with a stored token at the dialed URL |
| 2 | `InitConnection` or `Reject` | `401` for an unknown worker ID or any wrong token, `400` for any other first message |
| 3 | `InitAck` or `Reject` | The projects whose tokens the worker accepted, without a challenge round |

- The server will send its tokens over `wss://` URLs.
- A `ws://` URL is for local and operator setups only. The server will still send the tokens and log a warning.
- Workers check server tokens against the hashes in `services.gradient.worker.acceptedServerTokensFile` on the worker. Workers without the file accept every server.
- A reauth of a server-dialed session is answered with `AuthUpdate`, never with `AuthChallenge`.
- A reauth finding a new registration will close the session. The next dial will carry the new project's token.
- Projects granting a team's workers join the running session. All of them share the team worker's one token for the team ID.
- A registration may not reuse the worker ID of a team worker. Gradient.CI worker IDs belong to one registration or team worker only.
- The server will keep the last failure of every worker ID in memory as the worker's offline reason.
- The server will keep a failure before the token check only for a registered worker ID.

## Capabilities

A capability is active only when both sides support the capability. Only the server can set `core` and `cache`.

| Capability | Meaning |
|---|---|
| `core` | The server side of the protocol. Always on for the server, off for workers |
| `cache` | Serving as a binary cache. Always on for the server |
| `fetch` | Cloning repositories and prefetching flake inputs |
| `eval` | Running flake evaluations |
| `build` | Running Nix builds |
| `federate` | Reserved: negotiated in the handshake, no behavior yet |

Capability flags guard new features, not version numbers.

## Authorization

A **peer** is anything registering a worker, such as a project, a cache or a proxy. Both sides consent. The peer will register the worker ID, and the worker will hold the peer's token.

1. A peer registered the worker ID. The server will generate a token for that pair.
2. The worker's peers file (`services.gradient.worker.peersFile`) will hold `peer_id:token` lines.
3. The server will challenge every registering peer on connect. The server will check each token on its own.

| Peer | Granted Access |
|---|---|
| Project | Jobs from the project's tasks |
| Cache | Serving and pulling from the cache |
| Proxy | Membership in the proxy's worker pool |

- The session can survive some failed tokens with the remaining peers.
- Only a failure of all tokens will reject the session.
- A project without a cache subscription will move to `failed_peers` in the reply.
- The reply is `495` when no peer remained.
- **Reauth** can add peers without reconnecting.
- The server will send `AuthChallenge` to a worker-dialed session when a peer registered the worker.
- The worker will send `ReauthRequest` when its peers file changed.
- Both will end in `AuthUpdate`, and reauth will never revoke granted peers.
- One connection per worker ID. The server will reject a second connection with `496`.
- The server will drop a connection silent for `proto.workerHeartbeatTimeoutSecs` (120 s).
- A project's worker list will show a worker as live only when the worker authenticated for that project.

## Build Timeouts

Each `BuildSpec` will carry `timeout_secs` and `max_silent_secs`. The values are the derivation's own `timeout` and `maxSilent`. The fallbacks are `build.defaultTimeoutSecs` (14400) and `build.defaultMaxSilentSecs` (3600). A value of `0` will disable a limit. A build past a limit will end as `FailedTimeout`. Evaluations have no timeout.

## Server Restart

A restart can lose work in flight, never a queued job.

| Side | Behavior |
|---|---|
| Worker | Aborting every running job and reconnecting with backoff (1 s, doubling up to 60 s) |
| Worker | Sending a full handshake, then `RequestJobList` and one `RequestJob` per kind |
| Server | Dropping every report whose `job_id` and `assignment_id` the current session did not hand out |
| Server | `recover_interrupted_work` is closing open assignments and aborting running attempts before the first session. The pass is also re-queuing `Building` builds and re-evaluating interrupted evaluations |

## Graceful Shutdown

```mermaid
sequenceDiagram
    participant W as Worker
    participant S as Server
    S->>W: Draining
    Note left of W: stopping job requests
    W->>S: JobCompleted (in flight)
    S-xW: close
```

- The server will send `Draining` on `SIGTERM` and assign nothing more.
- The server will close each session once the worker is idle, after 20 s at the latest.
- Startup recovery will re-queue whatever the cut-off interrupted.
- A `Draining` message will end the session, never the worker process.
- Workers reconnect once the server is back.

## Version Agreement

```text
"GRAD" | oldest: u16 LE | newest: u16 LE
```

- Both sides send this 8-byte frame before their first message. The frame format will never change.
- The `ProtoSocket` type will agree on the first send or receive. Both sides are sending first and reading second.
- The agreed version is the lower of both newest versions. Every later frame is encoded for that version.
- Two ranges without overlap will end the session with close code `1002` and `no shared protocol version: ours 27..=27, peer 30..=31`.
- A first frame without `GRAD` came from protocol 26 or older. The session will end with `peer protocol is older than 27`.
- Both sides are logging the reason at the `warn` level.
- A server dialing a worker will keep the reason as the worker's offline reason.
- `gradient_wire::PROTO_VERSIONS` is the supported range, computed from the `#[proto]` annotations.
- The `writer.version()` call will expose the agreed version for feature checks.

## Changing a Message

| Change | Annotation | Effect |
|---|---|---|
| New field | `#[proto(28)]` | Field present since 28. The oldest supported version is rising to 28 |
| New field with a fallback | `#[proto(28, default)]` | Older peers are leaving the field out. Decoding is filling `Default::default()` |
| New variant | `#[proto(28)]` on the variant | Appended at the end. Sending the variant to a peer below 28 is an `EncodeError` |
| Removed or reordered field or variant, changed meaning | `#[proto(oldest = 30)]` on `ClientMessage` and `ServerMessage` | The oldest supported version is rising to 30 |

- A field without `default` is required. A forgotten `default` can cost compatibility, never correctness.
- Variants are append-only. A variant older than the one before is a compile error.
- The files `gradient-wire/schema/v{N}.txt` hold the wire shape of each supported version. The `schema` test will compare the files to the code.
- Running `cargo run --example wire_schema` will write the file of a new version and delete files below the oldest. Existing files are never rewritten.

## Cache Sessions

The `/cache/{cache}/proto` endpoint will offer the same frames as a read-only session for one cache.

| Aspect | Rule |
|---|---|
| Public cache | No credentials, unless `proto.anonymousCache.enable` is off |
| Private cache | `Authorization: GRAD<key>` on the upgrade request |
| Handshake | `InitConnection`, then `InitAck` with no peers. No challenge |
| Allowed | `CacheQuery` in `Normal` or `Pull` mode, `NarRequest`. The server is rejecting everything else with `403` |
| Limits | `proto.anonymousCache.maxConnectionsPerIp` (32) anonymous connections per IP. Every session is counting against `proto.maxConnections` (256). `SAFE_INFLIGHT_MESSAGE_SIZE` (2 MiB) is capping each message, enough for a `CacheQuery` of 1000 paths |
| Idle | Closed after 120 s without a NAR transfer |

## Implementation

- One pure handshake state machine in `gradient-wire/src/session/handshake.rs` will drive every session.
- The server will run `as_authority` for a worker dialing in and `as_dialer` for a worker the server dialed. The worker will run `as_peer` and `as_dialed`.
- The cache session will agree on a version like every other session.
- The code in `session/frame.rs` will split each socket into a typed reader and a writer. The writer will hold a control lane and a bulk lane.
- Control frames go first. A bulk batch will hold at most 256 KiB.
- A full bulk queue will never block control replies.
- Chunk payloads (`NarPush`, `UploadChunk`, `EvalCacheChunk`, `LogChunk`) reach the handler as slices of the frame, without a copy.
- The `client::dial` function will disable Nagle's algorithm on every socket. Small control frames go out without waiting for a delayed ACK.
