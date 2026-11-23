# Control channel

Last updated: 2026-11-24.

A running MetricCraft window can be driven by a local program over a TCP socket. It is off unless
you start the app with `--control`:

```bash
metriccraft --control [PROJECT]
```

The window then listens on `127.0.0.1` on a random port. Every request must carry a per-session
token. The port and token are in `control-<pid>.json` in the runtime folder:

| Platform | Runtime folder                                                            |
|----------|---------------------------------------------------------------------------|
| Linux    | `$XDG_RUNTIME_DIR/metriccraft`, else `~/.cache/metriccraft/run`           |
| macOS    | `~/Library/Caches/metriccraft/run`                                        |
| Windows  | `%LOCALAPPDATA%\metriccraft\run`                                          |

The file is `{"pid", "port", "token"}`, readable by you only (mode 0600 in a 0700 folder on Unix;
Windows keeps the per-user folder's access). It is removed when the window closes. Files left by
a session that died are dropped when the next one starts (nothing answers on their port).
The web build has no control channel.

## Protocol

One JSON object per line in each direction. A request line is at most 1 MiB.

```text
-> {"token": "…", "id": 7, "run": "insert.newPage", "params": {"name": "Q3"}}
<- {"id": 7, "ok": true, "result": null}
<- {"id": 7, "ok": false, "error": {"kind": "invalid_params", "message": "…"}}
```

- `run` is any [command](commands.md): an edit, a Read command (`document.describe`,
  `dax.evaluate`, `page.render`) or `batch`. `params` defaults to `{}`; `id` is optional, any JSON
  value, and comes back in the reply.
- Commands run on the same queue as the window's own, one at a time, as Origin Control (undo
  labels read `Control channel — …`). The window shows the change. Like every programmatic call
  they never open a dialog.
- A batch is all or nothing and one undo step.

Error kinds: `bad_json`, `bad_request`, `bad_token`, `too_large`, `stopped`, `unknown_command`,
`disabled`, `invalid_params`, `batch`, `read`, `failed`. An error never ends the connection,
except `bad_token` and `too_large`, which close it. At most 16 connections are served at once, and
a silent connection is closed after two minutes.
