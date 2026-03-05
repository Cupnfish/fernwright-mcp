use anyhow::{Result, anyhow};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};

use crate::bridge::BridgeServer;

const PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Clone, Copy)]
enum MessageFraming {
    ContentLength,
    JsonLine,
}

pub async fn run_stdio_server(bridge: BridgeServer) -> Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let mut reader = BufReader::new(stdin);
    let mut writer = BufWriter::new(stdout);

    loop {
        let (inbound, framing) = match read_message(&mut reader).await? {
            Some(pair) => pair,
            None => break,
        };

        if let Some(response) = handle_request(&bridge, inbound).await {
            write_message(&mut writer, &response, framing).await?;
        }
    }

    Ok(())
}

async fn handle_request(bridge: &BridgeServer, message: Value) -> Option<Value> {
    let id = message.get("id").cloned();
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();

    match method {
        "initialize" => {
            let request_id = id?;
            Some(rpc_result(
                request_id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {
                        "tools": {
                            "listChanged": false
                        }
                    },
                    "serverInfo": {
                        "name": "playwright-tab-bridge-rust",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            ))
        }
        "notifications/initialized" => None,
        "ping" => Some(rpc_result(id?, json!({}))),
        "tools/list" => Some(rpc_result(id?, json!({ "tools": tool_definitions() }))),
        "tools/call" => {
            let request_id = id?;
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            let result = match call_tool(bridge, params).await {
                Ok(payload) => payload,
                Err(err) => json!({
                    "content": [
                        {
                            "type": "text",
                            "text": format!("Tool error: {err:#}")
                        }
                    ],
                    "isError": true
                }),
            };

            Some(rpc_result(request_id, result))
        }
        _ => id.map(|request_id| rpc_error(request_id, -32601, "Method not found", None)),
    }
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "list_clients",
            "description": "List connected browser extension clients and cached tab snapshots.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        }),
        json!({
            "name": "list_tabs",
            "description": "List all tabs available from a connected browser client.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "client_id": { "type": "string", "description": "Optional specific extension client ID." }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "navigate_tab",
            "description": "Navigate an existing tab to a URL.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "client_id": { "type": "string" },
                    "tab_id": { "type": "integer" },
                    "url": { "type": "string" }
                },
                "required": ["tab_id", "url"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "activate_tab",
            "description": "Focus a tab and its window.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "client_id": { "type": "string" },
                    "tab_id": { "type": "integer" }
                },
                "required": ["tab_id"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "click",
            "description": "Wait for a selector and click it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "client_id": { "type": "string" },
                    "tab_id": { "type": "integer" },
                    "selector": { "type": "string" },
                    "timeout_ms": { "type": "integer" }
                },
                "required": ["tab_id", "selector"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "fill",
            "description": "Set value for an input-like element and dispatch change events.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "client_id": { "type": "string" },
                    "tab_id": { "type": "integer" },
                    "selector": { "type": "string" },
                    "value": { "type": "string" },
                    "timeout_ms": { "type": "integer" }
                },
                "required": ["tab_id", "selector", "value"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "press_key",
            "description": "Dispatch keydown/keyup on an element.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "client_id": { "type": "string" },
                    "tab_id": { "type": "integer" },
                    "selector": { "type": "string" },
                    "key": { "type": "string" },
                    "timeout_ms": { "type": "integer" }
                },
                "required": ["tab_id", "selector", "key"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "evaluate_js",
            "description": "Evaluate JavaScript in the tab's DOM context.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "client_id": { "type": "string" },
                    "tab_id": { "type": "integer" },
                    "script": { "type": "string" },
                    "args": { "type": "array" }
                },
                "required": ["tab_id", "script"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "extract_text",
            "description": "Extract visible text from an element selector.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "client_id": { "type": "string" },
                    "tab_id": { "type": "integer" },
                    "selector": { "type": "string" },
                    "max_length": { "type": "integer" }
                },
                "required": ["tab_id"],
                "additionalProperties": false
            }
        }),
    ]
}

async fn call_tool(bridge: &BridgeServer, params: Value) -> Result<Value> {
    let params_object = params
        .as_object()
        .ok_or_else(|| anyhow!("tools/call params must be an object"))?;
    let tool_name = get_required_string(params_object, "name")?;

    let arguments = params_object
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let args = arguments
        .as_object()
        .ok_or_else(|| anyhow!("Tool arguments must be an object"))?;

    let output = match tool_name.as_str() {
        "list_clients" => {
            let clients = bridge.list_clients().await;
            json!({ "clients": clients })
        }
        "list_tabs" => {
            let client_id = get_optional_string(args, "client_id");
            let response = bridge
                .request(client_id.as_deref(), "listTabs", json!({}))
                .await?;
            json!({
                "client_id": response.client_id,
                "result": response.payload
            })
        }
        "navigate_tab" => {
            let client_id = get_optional_string(args, "client_id");
            let tab_id = get_required_i64(args, "tab_id")?;
            let url = get_required_string(args, "url")?;

            let response = bridge
                .request(
                    client_id.as_deref(),
                    "navigate",
                    json!({
                        "tabId": tab_id,
                        "url": url,
                    }),
                )
                .await?;

            json!({
                "client_id": response.client_id,
                "result": response.payload
            })
        }
        "activate_tab" => {
            let client_id = get_optional_string(args, "client_id");
            let tab_id = get_required_i64(args, "tab_id")?;

            let response = bridge
                .request(
                    client_id.as_deref(),
                    "activateTab",
                    json!({ "tabId": tab_id }),
                )
                .await?;

            json!({
                "client_id": response.client_id,
                "result": response.payload
            })
        }
        "click" => {
            let client_id = get_optional_string(args, "client_id");
            let tab_id = get_required_i64(args, "tab_id")?;
            let selector = get_required_string(args, "selector")?;
            let timeout_ms = get_optional_i64(args, "timeout_ms");

            let mut params = json!({
                "tabId": tab_id,
                "selector": selector,
            });
            if let Some(timeout_ms) = timeout_ms {
                params["timeoutMs"] = json!(timeout_ms);
            }

            let response = bridge
                .request(client_id.as_deref(), "click", params)
                .await?;
            json!({
                "client_id": response.client_id,
                "result": response.payload
            })
        }
        "fill" => {
            let client_id = get_optional_string(args, "client_id");
            let tab_id = get_required_i64(args, "tab_id")?;
            let selector = get_required_string(args, "selector")?;
            let value = get_required_string(args, "value")?;
            let timeout_ms = get_optional_i64(args, "timeout_ms");

            let mut params = json!({
                "tabId": tab_id,
                "selector": selector,
                "value": value,
            });
            if let Some(timeout_ms) = timeout_ms {
                params["timeoutMs"] = json!(timeout_ms);
            }

            let response = bridge.request(client_id.as_deref(), "fill", params).await?;
            json!({
                "client_id": response.client_id,
                "result": response.payload
            })
        }
        "press_key" => {
            let client_id = get_optional_string(args, "client_id");
            let tab_id = get_required_i64(args, "tab_id")?;
            let selector = get_required_string(args, "selector")?;
            let key = get_required_string(args, "key")?;
            let timeout_ms = get_optional_i64(args, "timeout_ms");

            let mut params = json!({
                "tabId": tab_id,
                "selector": selector,
                "key": key,
            });
            if let Some(timeout_ms) = timeout_ms {
                params["timeoutMs"] = json!(timeout_ms);
            }

            let response = bridge
                .request(client_id.as_deref(), "press", params)
                .await?;
            json!({
                "client_id": response.client_id,
                "result": response.payload
            })
        }
        "evaluate_js" => {
            let client_id = get_optional_string(args, "client_id");
            let tab_id = get_required_i64(args, "tab_id")?;
            let script = get_required_string(args, "script")?;
            let args_array = args.get("args").cloned().unwrap_or_else(|| json!([]));

            let response = bridge
                .request(
                    client_id.as_deref(),
                    "evaluate",
                    json!({
                        "tabId": tab_id,
                        "script": script,
                        "args": args_array,
                    }),
                )
                .await?;

            json!({
                "client_id": response.client_id,
                "result": response.payload
            })
        }
        "extract_text" => {
            let client_id = get_optional_string(args, "client_id");
            let tab_id = get_required_i64(args, "tab_id")?;
            let selector =
                get_optional_string(args, "selector").unwrap_or_else(|| "body".to_owned());
            let max_length = get_optional_i64(args, "max_length");

            let mut params = json!({
                "tabId": tab_id,
                "selector": selector,
            });
            if let Some(max_length) = max_length {
                params["maxLength"] = json!(max_length);
            }

            let response = bridge
                .request(client_id.as_deref(), "extractText", params)
                .await?;

            json!({
                "client_id": response.client_id,
                "result": response.payload
            })
        }
        _ => {
            return Ok(json!({
                "content": [
                    {
                        "type": "text",
                        "text": format!("Unknown tool: {tool_name}")
                    }
                ],
                "isError": true
            }));
        }
    };

    Ok(json!({
        "content": [
            {
                "type": "text",
                "text": serde_json::to_string_pretty(&output)?
            }
        ]
    }))
}

fn get_required_string(map: &Map<String, Value>, key: &str) -> Result<String> {
    map.get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("'{key}' must be a non-empty string"))
}

fn get_optional_string(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key).and_then(Value::as_str).map(ToOwned::to_owned)
}

fn get_required_i64(map: &Map<String, Value>, key: &str) -> Result<i64> {
    map.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("'{key}' must be an integer"))
}

fn get_optional_i64(map: &Map<String, Value>, key: &str) -> Option<i64> {
    map.get(key).and_then(Value::as_i64)
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn rpc_error(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({
        "code": code,
        "message": message,
    });
    if let Some(data) = data {
        error["data"] = data;
    }

    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": error,
    })
}

async fn read_message<R>(reader: &mut BufReader<R>) -> Result<Option<(Value, MessageFraming)>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut first_line = String::new();
    let bytes_read = reader.read_line(&mut first_line).await?;
    if bytes_read == 0 {
        return Ok(None);
    }

    let trimmed = first_line.trim_end_matches(['\r', '\n']);
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        let message = serde_json::from_str::<Value>(trimmed)?;
        return Ok(Some((message, MessageFraming::JsonLine)));
    }

    let mut content_length: Option<usize> = None;
    if let Some((name, value)) = first_line.split_once(':') {
        if name.trim().eq_ignore_ascii_case("Content-Length") {
            let parsed = value.trim().parse::<usize>()?;
            content_length = Some(parsed);
        }
    }

    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            return Ok(None);
        }

        if line == "\r\n" || line == "\n" {
            break;
        }

        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                let parsed = value.trim().parse::<usize>()?;
                content_length = Some(parsed);
            }
        }
    }

    let content_length = content_length.ok_or_else(|| anyhow!("Missing Content-Length header"))?;
    let mut body = vec![0_u8; content_length];
    reader.read_exact(&mut body).await?;

    let message = serde_json::from_slice::<Value>(&body)?;
    Ok(Some((message, MessageFraming::ContentLength)))
}

async fn write_message<W>(
    writer: &mut BufWriter<W>,
    message: &Value,
    framing: MessageFraming,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let body = serde_json::to_string(message)?;
    match framing {
        MessageFraming::ContentLength => {
            writer
                .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
                .await?;
            writer.write_all(body.as_bytes()).await?;
        }
        MessageFraming::JsonLine => {
            writer.write_all(body.as_bytes()).await?;
            writer.write_all(b"\n").await?;
        }
    }
    writer.flush().await?;
    Ok(())
}
