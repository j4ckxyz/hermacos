# Hermacos

A native macOS client for a personal [Hermes Agent](https://hermes-agent.nousresearch.com).
Paste your dashboard address, sign in, chat. The Hermes server can be on this Mac, your LAN,
or a tailnet.

- **SwiftUI shell** (`app/`): Liquid Glass sidebar and composer, native markdown, code
  blocks, tables, images and link preview cards. macOS 26 or later.
- **Rust core** (`core/`): everything that is not pixels: sign-in, sessions, the live gateway
  connection, markdown parsing and streaming repair, stream pacing, link previews. Exposed to
  Swift through [UniFFI](https://mozilla.github.io/uniffi-rs/), so a Kotlin, C# or Python shell
  can reuse it unchanged.
- **Mock server** (`mock/`): a stand-in dashboard for development without an agent.

## Install

Needs macOS 26 (Tahoe) or later, on Apple silicon or Intel.

```sh
curl -fsSL https://raw.githubusercontent.com/j4ckxyz/hermacos/main/scripts/install.sh | sh
```

That downloads the latest release into `/Applications` and opens it. Run it again to update.

Or download `Hermacos-<version>.dmg` from
[Releases](https://github.com/j4ckxyz/hermacos/releases) and drag the app to Applications.
Releases are not notarized by Apple yet, so a copy downloaded in a browser is blocked the
first time: open **System Settings > Privacy & Security** and click **Open Anyway**. The
one-line install doesn't hit this.

## Build from source

Needs Rust and either Xcode or the Xcode Command Line Tools.

```sh
scripts/build.sh            # builds build/Hermacos.app
open build/Hermacos.app
```

`scripts/build.sh --debug` builds the Swift side unoptimised, which is faster to iterate on;
`--universal` builds for Apple silicon and Intel together, as releases do.

With only the Command Line Tools installed the script builds against the macOS 26 SDK: the
macOS 27 SDK makes `@State` a macro whose compiler plugin ships with full Xcode only.

The app requires macOS 26 because its interface is built on Liquid Glass and the other
SwiftUI APIs introduced there.

## Signing in

1. Paste the address you open the dashboard at (`https://hermes-vps.example.ts.net`; a pasted
   `/login?...` URL works too).
2. If the server has accounts, enter the dashboard username and password. Dashboards without
   accounts (loopback / trusted network) connect straight away. OAuth providers open the browser.

Tokens are kept in the login keychain; the access token is refreshed automatically.

How it talks to Hermes: the dashboard's native sign-in flow (`/auth/native/*`, RFC 8252 with
PKCE) for bearer tokens, the dashboard REST API for the session list and history, and the
`/api/ws` JSON-RPC gateway for live chat, the same surface the official desktop app uses.

## What it does

- **Chats from everywhere.** The sidebar lists conversations from every Hermes surface
  (desktop, CLI, Discord, Telegram, cron), newest first with the time of the last message,
  loading older ones as you scroll. Open any of them and carry on.
- **Type or paste anywhere.** With nothing focused, typing starts a message. ⌘V with files or
  an image on the clipboard attaches them from anywhere in the window; plain text goes to the
  message field.
- **Attachments.** The + button, paste, or drag and drop. Images are queued for the agent,
  PDFs are rendered to pages when the server can (otherwise sent as files), anything else is
  staged as a file. 25 MB each.
- **Rewrite a message.** Hover one of your own messages for Copy and Rewrite. Rewriting stops
  any reply in flight, cuts the conversation back to that message on the server, and sends the
  new text with the same attachments.
- **Activity, folded.** Thinking and tool calls collapse into one line showing the current
  step; click it to see every step, click again to fold it away.
- **Citations.** When a reply ends with a numbered source list, each `[n]` in the text becomes
  a chip that opens that source.
- **Usage.** The ring in the sidebar footer shows today's spend against a daily limit you set
  (dollars or tokens), or the plan allowance the server reports, or simply today's cost and
  tokens. Click it for the breakdown, a 14-day chart and the top models.

## Keyboard

| Shortcut | Action |
|:--|:--|
| ⌘N | New chat |
| any typing | Starts a message when no field is focused |
| ⌘V | Paste files, images or text into the message |
| ⌘F | Search chats |
| ⌘L | Focus the message field |
| Return / ⇧Return | Send / new line |
| ⌘. | Stop responding |
| ⇧⌘C | Copy last response |
| ⌘[ / ⌘] | Previous / next chat |
| ⌘R | Reload chats |
| ⌘I | Server status |
| ⌃⌘S | Toggle sidebar |
| ⌘, | Settings |

## Developing without a server

```sh
cargo run -p hermes-mock -- --port 9119          # sign in as admin / hermes
cargo run -p hermes-mock -- --port 9119 --open   # a dashboard without accounts
cargo test -p hermes-core                        # markdown, pacer, transcript, preview tests

# The core end to end from the terminal (works against a real server too):
cargo run -p hermes-core --example smoke -- http://127.0.0.1:9119 admin hermes "plan a trip"
```

The mock streams canned replies; prompts containing `trip`, `code`, `deploy` or `sources`
exercise tables and link previews, code blocks, tool calls with an approval request, and a
researched answer with thinking, searches and numbered citations. Flags: `--open` (no
accounts), `--plan` (reports a metered plan), `--many` (150 extra chats, for paging),
`--legacy-ws` (ticket-only WebSocket auth, like older servers).

```sh
# Attachments, rewriting, paging and usage, checked against the mock:
cargo run -p hermes-core --example features -- http://127.0.0.1:9119
```

Environment variables for driving the app in development:

| Variable | Effect |
|:--|:--|
| `HERMACOS_EPHEMERAL=1` | Don't read or write the keychain or saved account |
| `HERMACOS_AUTOLOGIN="url\|user\|password"` | Sign in at launch |
| `HERMACOS_SCRIPT=<steps>` | Then run newline-separated steps: type keys, paste, attach, send, rewrite, dump state as JSON. See `Support/Automation.swift` |
| `HERMACOS_APPEARANCE=light\|dark` | Pin the appearance |

Script steps post real key events to the app's own queue, so typing and ⌘V are exercised
end to end without Accessibility permission, and pastes use a private pasteboard.

## Layout

```
core/src/auth.rs        sign-in flows, token refresh
core/src/gateway.rs     /api/ws JSON-RPC: reconnect, heartbeat, events, agent requests
core/src/rest.rs        sessions, search, history, status
core/src/markdown.rs    markdown -> flat render blocks; repair of half-streamed markdown
core/src/pacer.rs       smooths bursty token streams; per-character fade timing
core/src/preview.rs     link previews from Open Graph tags
app/Sources/Hermacos    the SwiftUI app
app/Sources/HermesCore  generated UniFFI bindings (build output)
```

## Releasing

A version tag is a release. Pushing one makes GitHub Actions test the core, build a universal
app, package it and publish a GitHub release with the disk image, a zip and checksums:

```sh
git tag v0.2.0
git push origin v0.2.0
```

The tag is the version the app reports. `0.x` tags and tags with a suffix (`v0.3.0-beta.1`)
are published as pre-releases. Every push to `main` runs the same tests without releasing.

To ship without the Gatekeeper warning, add a Developer ID certificate and notary credentials
as repository secrets; `.github/workflows/release.yml` lists them and uses them when present.
