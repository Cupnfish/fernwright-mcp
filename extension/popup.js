const POLL_INTERVAL_MS = 1_500;

const stateBadge = document.getElementById("stateBadge");
const summaryNode = document.getElementById("summary");
const clientIdNode = document.getElementById("clientId");
const serverUrlNode = document.getElementById("serverUrl");
const tabCountNode = document.getElementById("tabCount");
const heartbeatAgeNode = document.getElementById("heartbeatAge");
const statusNode = document.getElementById("status");

const reconnectButton = document.getElementById("reconnectButton");
const refreshButton = document.getElementById("refreshButton");
const optionsButton = document.getElementById("optionsButton");

let pollTimer = null;

function formatAge(timestamp) {
  if (!timestamp) {
    return "never";
  }

  const value = new Date(timestamp).getTime();
  if (!Number.isFinite(value)) {
    return "invalid";
  }

  const deltaSeconds = Math.max(0, Math.round((Date.now() - value) / 1_000));
  if (deltaSeconds < 2) {
    return "just now";
  }

  if (deltaSeconds < 60) {
    return `${deltaSeconds}s ago`;
  }

  const minutes = Math.floor(deltaSeconds / 60);
  if (minutes < 60) {
    return `${minutes}m ago`;
  }

  const hours = Math.floor(minutes / 60);
  return `${hours}h ago`;
}

function renderStateBadge(status) {
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
  renderStateBadge(status);

  const summaryParts = [];
  summaryParts.push(`Socket: ${status.wsState || "unknown"}`);
  summaryParts.push(`Reconnect attempts: ${status.reconnectAttempts ?? 0}`);
  summaryNode.textContent = summaryParts.join(" | ");

  clientIdNode.textContent = status.clientId || "-";
  serverUrlNode.textContent = status.serverUrl || status.configuredServerUrl || "-";
  tabCountNode.textContent = String(status.tabCount ?? 0);
  heartbeatAgeNode.textContent = formatAge(status.lastHeartbeatAt);

  statusNode.textContent = JSON.stringify(status, null, 2);
}

function renderError(errorMessage) {
  renderStateBadge({ connected: false, connecting: false });
  summaryNode.textContent = "Status unavailable";
  statusNode.textContent = JSON.stringify({ error: errorMessage }, null, 2);
}

async function fetchStatus() {
  const response = await chrome.runtime.sendMessage({ type: "bridge:status" });
  if (!response) {
    renderError("Service worker unavailable");
    return;
  }

  renderStatus(response);
}

async function refreshAll() {
  try {
    await chrome.runtime.sendMessage({ type: "bridge:syncTabs" });
  } catch (err) {
    console.warn("Tab sync request failed", err);
  }

  await fetchStatus();
}

async function reconnect() {
  const response = await chrome.runtime.sendMessage({ type: "bridge:reconnect" });
  if (!response?.ok) {
    renderError(response?.error || "Reconnect failed");
    return;
  }

  await refreshAll();
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

reconnectButton.addEventListener("click", () => {
  reconnect().catch((err) => renderError(String(err)));
});

refreshButton.addEventListener("click", () => {
  refreshAll().catch((err) => renderError(String(err)));
});

optionsButton.addEventListener("click", () => {
  chrome.runtime.openOptionsPage();
});

window.addEventListener("unload", () => {
  stopPolling();
});

(async () => {
  try {
    await refreshAll();
    startPolling();
  } catch (err) {
    renderError(String(err));
    startPolling();
  }
})();
