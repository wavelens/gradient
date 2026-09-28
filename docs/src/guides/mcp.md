# Connect an AI Assistant

Evaluations, builds and build logs readable by any [Model Context Protocol](https://modelcontextprotocol.io) client (Claude Code, Claude Desktop, Cursor), through `gradient mcp`. An assistant finds the failed build and reads the log line that broke the build.

**Requirements:**

- The CLI: `nix shell github:wavelens/gradient#gradient-cli`

## 1. Log In

```sh
gradient login https://gradient.example.com
```

Every tool call runs as this user and sees only the user's projects. `gradient project select <name>` sets the default project for tools that take one.

## 2. Add the Server to the Client

=== "Claude Code"

    ```sh
    claude mcp add gradient -- gradient mcp
    ```

=== "JSON config"

    ```json
    {
      "mcpServers": {
        "gradient": {
          "command": "gradient",
          "args": ["mcp"]
        }
      }
    }
    ```

## Verify Deployment

Ask the assistant: "Why did the last evaluation of `web-app` fail?" The assistant walks down the tools:

```mermaid
flowchart LR
    evals[list_evaluations] --> builds[list_builds] --> log[search_build_log / get_build_log]
```

## Tools

| Tool | Returns |
|---|---|
| `list_projects`, `list_tasks` | The user's projects and their tasks |
| `list_evaluations`, `get_evaluation` | A task's evaluations with build counts; one evaluation with commit, status and error |
| `list_builds`, `get_build` | The builds of an evaluation; one build with derivation, system and outputs |
| `get_build_log` | A log, whole or by line range; more than 10 lines go to a temp file whose path is returned |
| `search_build_log` | Matching log lines with line numbers |

## Control Tools

`gradient mcp --control` lets the assistant act on the CI, limited by the user's project permissions:

| Tool | Does |
|---|---|
| `start_evaluation` | Starts an evaluation of a task, optionally at a commit |
| `abort_evaluation` | Aborts the running and queued builds of an evaluation |
| `watch_evaluation` | Waits until an evaluation finishes, up to `timeout_seconds` (600 by default, at most 3600) |

Without `--control`, the server is read-only.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `Server URL not set` | Run `gradient login` first |
| Client reports a protocol error in a wrapper script | The server speaks JSON-RPC on stdout; keep stderr out of stdout |

## Next Steps

- [Evaluations and Builds](../concepts/evaluations-and-builds.md): what the tools return
- [CLI](../reference/cli.md): the other `gradient` commands
