use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::protocol::Message;

#[derive(Clone)]
pub struct BridgeServer {
    state: Arc<BridgeState>,
    request_timeout: Duration,
}

struct BridgeState {
    clients: RwLock<HashMap<String, ClientHandle>>,
    pending: Mutex<HashMap<String, PendingRequest>>,
    sequence: AtomicU64,
}

#[derive(Clone)]
struct ClientHandle {
    client_id: String,
    connection_id: u64,
    sender: mpsc::UnboundedSender<Message>,
    user_agent: Option<String>,
    extension_version: Option<String>,
    tabs: Vec<Value>,
    connected_at_ms: u64,
    last_seen_ms: u64,
}

struct PendingRequest {
    client_id: String,
    tx: oneshot::Sender<Result<Value, String>>,
}

#[derive(Debug, Serialize)]
pub struct ClientSnapshot {
    pub client_id: String,
    pub connection_id: u64,
    pub user_agent: Option<String>,
    pub extension_version: Option<String>,
    pub connected_at_ms: u64,
    pub last_seen_ms: u64,
    pub tab_count: usize,
    pub tabs: Vec<Value>,
}

#[derive(Debug, Serialize)]
pub struct BridgeResponse {
    pub client_id: String,
    pub payload: Value,
}

impl BridgeServer {
    pub fn new(request_timeout: Duration) -> Self {
        Self {
            state: Arc::new(BridgeState {
                clients: RwLock::new(HashMap::new()),
                pending: Mutex::new(HashMap::new()),
                sequence: AtomicU64::new(1),
            }),
            request_timeout,
        }
    }

    pub async fn run_ws_listener(&self, addr: &str) -> Result<()> {
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("Failed to bind websocket listener to {addr}"))?;

        loop {
            let (stream, peer_addr) = listener.accept().await?;
            let bridge = self.clone();

            tokio::spawn(async move {
                if let Err(err) = bridge.handle_connection(stream).await {
                    eprintln!("[{peer_addr}] bridge connection error: {err:#}");
                }
            });
        }
    }

    pub async fn list_clients(&self) -> Vec<ClientSnapshot> {
        let clients = self.state.clients.read().await;
        let mut snapshots: Vec<ClientSnapshot> = clients
            .values()
            .map(|client| ClientSnapshot {
                client_id: client.client_id.clone(),
                connection_id: client.connection_id,
                user_agent: client.user_agent.clone(),
                extension_version: client.extension_version.clone(),
                connected_at_ms: client.connected_at_ms,
                last_seen_ms: client.last_seen_ms,
                tab_count: client.tabs.len(),
                tabs: client.tabs.clone(),
            })
            .collect();

        snapshots.sort_by(|a, b| a.client_id.cmp(&b.client_id));
        snapshots
    }

    pub async fn request(
        &self,
        target_client: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<BridgeResponse> {
        let (client_id, sender) = self.pick_client(target_client).await?;
        let request_id = format!(
            "req-{}",
            self.state.sequence.fetch_add(1, Ordering::Relaxed)
        );

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.state.pending.lock().await;
            pending.insert(
                request_id.clone(),
                PendingRequest {
                    client_id: client_id.clone(),
                    tx,
                },
            );
        }

        let message = json!({
            "type": "request",
            "id": request_id,
            "method": method,
            "params": params,
        });

        if sender
            .send(Message::Text(message.to_string().into()))
            .is_err()
        {
            let mut pending = self.state.pending.lock().await;
            pending.remove(&request_id);
            return Err(anyhow!(
                "Selected client disconnected before request could be sent"
            ));
        }

        let wait = timeout(self.request_timeout, rx).await;
        let response = match wait {
            Ok(Ok(Ok(payload))) => payload,
            Ok(Ok(Err(error_message))) => {
                return Err(anyhow!(
                    "Client '{client_id}' rejected request: {error_message}"
                ));
            }
            Ok(Err(_recv_err)) => {
                let mut pending = self.state.pending.lock().await;
                pending.remove(&request_id);
                return Err(anyhow!(
                    "Client '{client_id}' disconnected while waiting for response"
                ));
            }
            Err(_elapsed) => {
                let mut pending = self.state.pending.lock().await;
                pending.remove(&request_id);
                return Err(anyhow!(
                    "Timed out waiting for response from client '{client_id}'"
                ));
            }
        };

        Ok(BridgeResponse {
            client_id,
            payload: response,
        })
    }

    async fn pick_client(
        &self,
        target_client: Option<&str>,
    ) -> Result<(String, mpsc::UnboundedSender<Message>)> {
        let clients = self.state.clients.read().await;

        if clients.is_empty() {
            return Err(anyhow!(
                "No extension clients connected. Load the extension and verify the WebSocket URL."
            ));
        }

        if let Some(client_id) = target_client {
            let selected = clients
                .get(client_id)
                .ok_or_else(|| anyhow!("Client '{client_id}' is not connected"))?;
            return Ok((selected.client_id.clone(), selected.sender.clone()));
        }

        if clients.len() == 1 {
            let selected = clients
                .values()
                .next()
                .ok_or_else(|| anyhow!("No extension clients connected"))?;
            return Ok((selected.client_id.clone(), selected.sender.clone()));
        }

        let mut available: Vec<String> = clients.keys().cloned().collect();
        available.sort();
        Err(anyhow!(
            "Multiple extension clients connected. Pass client_id explicitly. Connected clients: {}",
            available.join(", ")
        ))
    }

    async fn handle_connection(&self, stream: TcpStream) -> Result<()> {
        let websocket = accept_async(stream).await?;
        let (mut writer, mut reader) = websocket.split();

        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

        let write_task = tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                if writer.send(message).await.is_err() {
                    break;
                }
            }
        });

        let connection_id = self.state.sequence.fetch_add(1, Ordering::Relaxed);
        let mut claimed_client_id: Option<String> = None;

        while let Some(message) = reader.next().await {
            let message = message?;

            match message {
                Message::Text(text) => {
                    let payload: Value = match serde_json::from_str(&text) {
                        Ok(value) => value,
                        Err(err) => {
                            eprintln!("Ignoring malformed bridge payload: {err}");
                            continue;
                        }
                    };
                    let now_ms = unix_time_ms();

                    let message_type = payload
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default();

                    match message_type {
                        "hello" => {
                            let client_id = payload
                                .get("clientId")
                                .and_then(Value::as_str)
                                .ok_or_else(|| anyhow!("Missing clientId in hello message"))?
                                .to_owned();
                            let user_agent = payload
                                .get("userAgent")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned);
                            let extension_version = payload
                                .get("extensionVersion")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned);

                            let handle = ClientHandle {
                                client_id: client_id.clone(),
                                connection_id,
                                sender: tx.clone(),
                                user_agent,
                                extension_version,
                                tabs: Vec::new(),
                                connected_at_ms: now_ms,
                                last_seen_ms: now_ms,
                            };

                            self.state
                                .clients
                                .write()
                                .await
                                .insert(client_id.clone(), handle);
                            claimed_client_id = Some(client_id);
                        }
                        "event" => {
                            let event_name = payload
                                .get("event")
                                .and_then(Value::as_str)
                                .unwrap_or_default();

                            if let Some(client_id) = &claimed_client_id {
                                let mut clients = self.state.clients.write().await;
                                if let Some(client) = clients.get_mut(client_id) {
                                    client.last_seen_ms = now_ms;

                                    if event_name == "tabsChanged" {
                                        let tabs = payload
                                            .get("data")
                                            .and_then(|data| data.get("tabs"))
                                            .and_then(Value::as_array)
                                            .cloned()
                                            .unwrap_or_default();
                                        client.tabs = tabs;
                                    }
                                }
                            }

                            if event_name == "heartbeat" {
                                let ack = json!({
                                    "type": "event",
                                    "event": "heartbeatAck",
                                    "data": {
                                        "atMs": now_ms
                                    }
                                });
                                let _ = tx.send(Message::Text(ack.to_string().into()));
                            }
                        }
                        "response" => {
                            if let Some(client_id) = &claimed_client_id {
                                let mut clients = self.state.clients.write().await;
                                if let Some(client) = clients.get_mut(client_id) {
                                    client.last_seen_ms = now_ms;
                                }
                            }

                            let request_id = payload
                                .get("id")
                                .and_then(Value::as_str)
                                .ok_or_else(|| anyhow!("Missing id in response message"))?
                                .to_owned();
                            let ok = payload.get("ok").and_then(Value::as_bool).unwrap_or(false);

                            let pending = {
                                let mut map = self.state.pending.lock().await;
                                map.remove(&request_id)
                            };

                            if let Some(pending_request) = pending {
                                if ok {
                                    let value =
                                        payload.get("result").cloned().unwrap_or(Value::Null);
                                    let _ = pending_request.tx.send(Ok(value));
                                } else {
                                    let error_text = payload
                                        .get("error")
                                        .and_then(Value::as_str)
                                        .unwrap_or("Unknown bridge error")
                                        .to_owned();
                                    let _ = pending_request.tx.send(Err(error_text));
                                }
                            }
                        }
                        _ => {
                            eprintln!("Ignoring unsupported bridge message type: {message_type}");
                        }
                    }
                }
                Message::Close(_) => {
                    break;
                }
                Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => {}
            }
        }

        if let Some(client_id) = claimed_client_id {
            let mut clients = self.state.clients.write().await;
            let should_remove = clients
                .get(&client_id)
                .map(|client| client.connection_id == connection_id)
                .unwrap_or(false);

            if should_remove {
                clients.remove(&client_id);
            }

            let mut pending = self.state.pending.lock().await;
            let stale_keys: Vec<String> = pending
                .iter()
                .filter_map(|(request_id, item)| {
                    if item.client_id == client_id {
                        Some(request_id.clone())
                    } else {
                        None
                    }
                })
                .collect();

            for request_id in stale_keys {
                if let Some(item) = pending.remove(&request_id) {
                    let _ = item.tx.send(Err(
                        "Bridge client disconnected before responding".to_owned()
                    ));
                }
            }
        }

        write_task.abort();
        Ok(())
    }
}

fn unix_time_ms() -> u64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    elapsed.as_millis() as u64
}
