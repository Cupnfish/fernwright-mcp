# Playwright MCP Chrome Tab Bridge (Rust + Extension)

This project implements an open alternative to closed-source "share browser tabs with MCP" extensions.

It has two components:

1. A Chrome extension (Manifest V3) that runs in your real browser profile and can automate existing tabs using `chrome.tabs` + `chrome.scripting`.
2. A Rust MCP server that exposes tools over stdio and forwards tool calls to the extension over local WebSocket.

## Why this structure

This split is the most practical architecture for your requirements:

- Browser control and session reuse must run inside the browser extension context.
- MCP transport for AI clients is most reliable as a local stdio server process.
- Rust is used for the MCP/tool server exactly as requested.
- WebSocket is a clean, local-only bridge between these two runtime boundaries.

## Project layout

```text
.
├── extension/
│   ├── manifest.json         # MV3 extension manifest
│   ├── service_worker.js     # WebSocket bridge + tab/action handlers
│   ├── popup.html            # Toolbar popup UI
│   ├── popup.js              # Live connection status UI logic
│   ├── options.html          # Extension options UI
│   └── options.js            # Server URL + reconnect/status controls
├── src/
│   ├── main.rs               # Process startup + WebSocket listener + MCP stdio loop
│   ├── bridge.rs             # Extension client registry + request/response bridge
│   └── mcp.rs                # MCP protocol framing + tool definitions + dispatch
└── Cargo.toml
```

## What it supports

The Rust MCP server currently exposes these tools:

- `list_clients`
- `list_tabs`
- `navigate_tab`
- `activate_tab`
- `click`
- `fill`
- `press_key`
- `evaluate_js`
- `extract_text`

These tools operate on tabs from your signed-in browser profile via the extension.

## Run the Rust MCP server

```bash
cargo run
```

Environment variables:

- `PLAYWRIGHT_MCP_BRIDGE_ADDR` (default: `127.0.0.1:17373`)
- `PLAYWRIGHT_MCP_REQUEST_TIMEOUT_MS` (default: `60000`)

The server listens for extension bridge connections at `ws://127.0.0.1:17373` by default.

## Install the extension

1. Open `chrome://extensions`.
2. Enable Developer Mode.
3. Click "Load unpacked".
4. Select this repo's `extension/` folder.
5. Click the extension icon to open the popup and confirm connection status.
6. Click `Open settings` from the popup and verify WebSocket URL is `ws://127.0.0.1:17373` (or your custom address).
7. Start the Rust server (`cargo run`).
8. Click `Reconnect` in the popup or options page if needed.

The popup and settings pages auto-refresh their status and tab counters while open.

## MCP client configuration example

For an MCP client that launches stdio servers, configure this repo as a server command.

Example shape (adjust to your client format):

```json
{
  "mcpServers": {
    "playwright-tab-bridge": {
      "command": "cargo",
      "args": ["run", "--quiet"],
      "cwd": "/absolute/path/to/playright-mcp"
    }
  }
}
```

## Quick MCP client smoke test (Inspector CLI)

Use the official Inspector CLI to test stdio MCP connectivity from terminal.

1. Ensure no other `playright-mcp` process is already listening on `127.0.0.1:17373`.
2. Ensure extension WebSocket URL is `ws://127.0.0.1:17373`.
3. Run:

```bash
# 1) MCP handshake + tool discovery
npx -y @modelcontextprotocol/inspector --cli --method tools/list ./target/debug/playright-mcp

# 2) Check extension bridge clients
npx -y @modelcontextprotocol/inspector --cli --method tools/call --tool-name list_clients ./target/debug/playright-mcp

# 3) Check visible tabs from connected browser
npx -y @modelcontextprotocol/inspector --cli --method tools/call --tool-name list_tabs ./target/debug/playright-mcp
```

If `list_clients` returns an empty list, the extension is not connected to the same server instance.

If multiple browser extension clients are connected, pass `client_id` explicitly in tool arguments.
This prevents accidental cross-client tab ID mismatches.

## Security model

- Keep the Rust server bound to localhost only.
- The extension executes DOM actions in pages you have open; this includes authenticated sessions.
- `evaluate_js` executes arbitrary script text in tab context. Use only with trusted MCP clients.

## Notes

- This is a pragmatic baseline implementation for real-session browser automation through MCP.
- It does not export raw cookie data; it performs actions in your actual tabs.
- Extension service worker sends heartbeat keepalive events to reduce idle websocket drops.
- Multiple extension clients are supported; pass `client_id` to pin actions to one browser context.
