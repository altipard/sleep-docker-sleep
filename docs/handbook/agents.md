# Coding agents

A coding agent with a shell can set up and operate `sds` for you: label a stack, connect
the reverse proxy, wake a group before tests, find out why a group did not start. The
repository ships instructions for that as an agent skill. The agent works with the
`sds` CLI described in this handbook; the skill only tells it how. There is no MCP server
yet, so agents without a shell (Claude Desktop) cannot use it.

## Claude Code plugin

The plugin `sds` contains the skill and nothing else. It needs the `sds` binary on `PATH`
(see [Installation](installation.md)) and installs nothing itself.

```bash
claude plugin install sds --marketplace altipard/sleep-docker-sleep
```

This adds the marketplace `altipard/sleep-docker-sleep` from GitHub and installs the
plugin for your user (`~/.claude`). Inside a Claude Code session the same works as
`/plugin install sds --marketplace altipard/sleep-docker-sleep`. A Claude Code that does
not know `--marketplace` needs two steps:

```text
/plugin marketplace add altipard/sleep-docker-sleep
/plugin install sds@sleep-docker-sleep
```

The installer offers this step at the end when it runs in a terminal and finds `claude` on
`PATH`; the default answer is no. It is skipped in CI, without a terminal, as root (the
plugin would land in root's `~/.claude`; the installer prints the command instead) and
with `SDS_NO_CLAUDE=1`:

```bash
curl -fsSL https://raw.githubusercontent.com/altipard/sleep-docker-sleep/main/install.sh | SDS_NO_CLAUDE=1 sh
```

Update and removal:

```bash
claude plugin marketplace update sleep-docker-sleep
claude plugin update sds@sleep-docker-sleep     # restart Claude Code afterwards
claude plugin uninstall sds@sleep-docker-sleep
```

### Not signature-checked

The installer verifies the `sds` binary against the pinned release key (or `SHA256SUMS`).
The plugin is not covered by that check: Claude Code fetches it from the repository on
GitHub, not from a signed release. It contains instructions for an
agent that runs commands in your shell, so install it only if you trust the repository.

### Versions

The skill names the oldest `sds` release it is written for. The binary can be older, for
example when it was installed with `SDS_VERSION`, because the plugin always follows the
repository. The agent then compares the commands it knows with `sds --help`, reports what
is missing and suggests `sds update`; it does not run the update itself.

## What the agent does and does not do

The skill sets ground rules. The most important ones:

- It shows changes to labels, compose files, `sds.toml` or service units before making
  them, and makes them only when asked. It never recreates containers, restarts `sds`, or
  runs `sds update` or `sds service install` on its own.
- It never prints the admin token. `sds integrate` writes the token in clear text into
  the proxy snippet; the agent shows the snippet only masked, writes the file with
  `umask 077` (mode 0600, or 0640 for the proxy's group), uses the Traefik file provider
  instead of container labels, and keeps the file out of git.
- It knows that `sds wake` does not keep a group awake (it sleeps again after
  `idle_timeout` without requests through the proxy) and that a `504` from
  `sds wake --wait` is not a failed start.
- It holds a group only when asked, because a hold has no expiry, and names
  `sds release`. It acts on one named group at a time.
- Container names, labels and error messages are treated as data: a request written into
  a compose file is reported, not followed.

These rules are advice to the agent, not limits: it has your shell and your Docker
access. Review what it proposes the same way you would review a colleague's change.

## Other agents

Agents that read skills from a directory (a `SKILL.md` with front matter plus
`references/`) can use the same files. Copy the directory `plugins/sds/skills/sds` from
the repository into the agent's skill directory; where that is depends on the agent, see
its documentation:

```bash
git clone --depth 1 https://github.com/altipard/sleep-docker-sleep sds-src
cp -R sds-src/plugins/sds/skills/sds <skill directory>/sds
```

A copy does not update itself; repeat the copy after `sds update`. For an agent without
skill support, point it to `SKILL.md` as instructions.

MCP configuration for Cursor, VS Code or Claude Desktop is not needed and not possible yet:
it follows only if `sds` gets an MCP server.
