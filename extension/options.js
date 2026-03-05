const DEFAULT_SERVER_URL = "ws://127.0.0.1:17373";
const POLL_INTERVAL_MS = 2_000;

const serverUrlInput = document.getElementById("serverUrl");
const saveButton = document.getElementById("saveButton");
const reconnectButton = document.getElementById("reconnectButton");
const refreshStatusButton = document.getElementById("refreshStatusButton");
const syncTabsButton = document.getElementById("syncTabsButton");

const stateBadge = document.getElementById("stateBadge");
const statusSummary = document.getElementById("statusSummary");
const lastUpdate = document.getElementById("lastUpdate");
const statusNode = document.getElementById("status");

let pollTimer = null;

function setBadge(status) {
  if (status.connected) {
    stateBadge.className = "badge connected";
    stateBadge.textContent = "Connected";
    return;
  }

  if (status.connecting) {
    stateBadge.className = "badge connecting";
    stateBadge.textContent = "Connecting";
    return;
  }

  stateBadge.className = "badge disconnected";
  stateBadge.textContent = "Disconnected";
}

function renderStatus(status) {
  setBadge(status);
  statusSummary.textContent = `Socket=${status.wsState || "unknown"} | Tabs=${status.tabCount ?? 0} | Reconnect attempts=${status.reconnectAttempts ?? 0}`;
  lastUpdate.textContent = `Last update: ${new Date().toLocaleTimeString()}`;
  statusNode.textContent = JSON.stringify(status, null, 2);
}

function renderMessage(message) {
  statusNode.textContent = JSON.stringify({ message }, null, 2);
}

function renderError(error) {
  setBadge({ connected: false, connecting: false });
  statusSummary.textContent = "Status unavailable";
  lastUpdate.textContent = `Last update: ${new Date().toLocaleTimeString()}`;
  statusNode.textContent = JSON.stringify({ error }, null, 2);
}

async function loadSettings() {
  const { serverUrl } = await chrome.storage.local.get("serverUrl");
  serverUrlInput.value = serverUrl || DEFAULT_SERVER_URL;
}

async function saveSettings() {
  const serverUrl = serverUrlInput.value.trim() || DEFAULT_SERVER_URL;
  await chrome.storage.local.set({ serverUrl });
  renderMessage(`Saved server URL: ${serverUrl}`);
}

async function fetchStatus() {
  const response = await chrome.runtime.sendMessage({ type: "bridge:status" });
  if (!response) {
    renderError("Service worker unavailable");
    return;
  }

  renderStatus(response);
}

async function reconnect() {
  const response = await chrome.runtime.sendMessage({ type: "bridge:reconnect" });
  if (!response?.ok) {
    renderError(response?.error || "Reconnect failed");
    return;
  }

  await fetchStatus();
}

async function syncTabs() {
  const response = await chrome.runtime.sendMessage({ type: "bridge:syncTabs" });
  if (!response?.ok) {
    renderError(response?.error || "Sync tabs failed");
    return;
  }

  await fetchStatus();
}

function startPolling() {
  if (pollTimer) {
    clearInterval(pollTimer);
  }

  pollTimer = setInterval(() => {
    fetchStatus().catch((err) => renderError(String(err)));
  }, POLL_INTERVAL_MS);
}

function stopPolling() {
  if (!pollTimer) {
    return;
  }

  clearInterval(pollTimer);
  pollTimer = null;
}

saveButton.addEventListener("click", () => {
  saveSettings()
    .then(() => reconnect())
    .catch((err) => renderError(String(err)));
});

reconnectButton.addEventListener("click", () => {
  reconnect().catch((err) => renderError(String(err)));
});

refreshStatusButton.addEventListener("click", () => {
  fetchStatus().catch((err) => renderError(String(err)));
});

syncTabsButton.addEventListener("click", () => {
  syncTabs().catch((err) => renderError(String(err)));
});

window.addEventListener("unload", () => {
  stopPolling();
});

(async () => {
  try {
    await loadSettings();
    await fetchStatus();
    startPolling();
  } catch (err) {
    renderError(String(err));
    startPolling();
  }
})();
