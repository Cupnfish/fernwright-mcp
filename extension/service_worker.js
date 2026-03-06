const DEFAULT_SERVER_URL = "ws://127.0.0.1:17373";
const RECONNECT_BASE_DELAY_MS = 2_000;
const RECONNECT_MAX_DELAY_MS = 15_000;
const HEARTBEAT_INTERVAL_MS = 15_000;
const TABS_SYNC_INTERVAL_MS = 30_000;

let ws = null;
let reconnectTimer = null;
let heartbeatTimer = null;
let tabsSyncTimer = null;
let tabsChangedTimer = null;
let connectInFlight = null;

let clientId = null;
let configuredServerUrl = DEFAULT_SERVER_URL;
let reconnectGeneration = 0;
let reconnectAttempts = 0;
let isConnecting = false;

let lastConnectedAt = null;
let lastDisconnectedAt = null;
let lastMessageAt = null;
let lastHeartbeatAt = null;
let lastError = null;

let cachedTabs = [];
let tabsUpdatedAt = null;

async function init() {
  clientId = await ensureClientId();
  await loadConfiguredServerUrl();
  await connect();
}

async function ensureClientId() {
  const { clientId: storedId } = await chrome.storage.local.get("clientId");
  if (storedId) {
    return storedId;
  }

  const nextId = crypto.randomUUID();
  await chrome.storage.local.set({ clientId: nextId });
  return nextId;
}

async function loadConfiguredServerUrl() {
  const { serverUrl } = await chrome.storage.local.get("serverUrl");
  configuredServerUrl = serverUrl || DEFAULT_SERVER_URL;
  return configuredServerUrl;
}

function nowIso() {
  return new Date().toISOString();
}

function formatError(err) {
  if (!err) {
    return null;
  }

  if (err instanceof Error) {
    return err.message;
  }

  if (typeof err === "string") {
    return err;
  }

  return JSON.stringify(err);
}

function currentSocketState() {
  if (!ws) {
    return "disconnected";
  }

  switch (ws.readyState) {
    case WebSocket.CONNECTING:
      return "connecting";
    case WebSocket.OPEN:
      return "open";
    case WebSocket.CLOSING:
      return "closing";
    case WebSocket.CLOSED:
      return "closed";
    default:
      return "unknown";
  }
}

function isSocketConnected() {
  return Boolean(ws && ws.readyState === WebSocket.OPEN);
}

function clearReconnectTimer() {
  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
}

function clearIntervalTimer(timer) {
  if (timer) {
    clearInterval(timer);
  }
}

function stopRuntimeLoops() {
  clearIntervalTimer(heartbeatTimer);
  clearIntervalTimer(tabsSyncTimer);
  heartbeatTimer = null;
  tabsSyncTimer = null;
}

function scheduleReconnect() {
  clearReconnectTimer();

  const exponent = Math.max(0, reconnectAttempts - 1);
  const delayMs = Math.min(RECONNECT_BASE_DELAY_MS * (2 ** exponent), RECONNECT_MAX_DELAY_MS);

  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connect().catch((err) => {
      lastError = formatError(err);
      console.error("Reconnect failed", err);
      reconnectAttempts += 1;
      scheduleReconnect();
    });
  }, delayMs);
}

function startHeartbeatLoop() {
  clearIntervalTimer(heartbeatTimer);

  const sendHeartbeat = () => {
    if (!isSocketConnected()) {
      return;
    }

    lastHeartbeatAt = nowIso();
    const sent = sendMessage({
      type: "event",
      event: "heartbeat",
      data: {
        at: lastHeartbeatAt,
      },
    });

    if (!sent) {
      scheduleReconnect();
    }
  };

  sendHeartbeat();
  heartbeatTimer = setInterval(sendHeartbeat, HEARTBEAT_INTERVAL_MS);
}

function startTabsSyncLoop() {
  clearIntervalTimer(tabsSyncTimer);

  tabsSyncTimer = setInterval(() => {
    emitTabsChanged().catch((err) => {
      console.warn("Periodic tabs sync failed", err);
    });
  }, TABS_SYNC_INTERVAL_MS);
}

function sendMessage(message) {
  if (!isSocketConnected()) {
    return false;
  }

  ws.send(JSON.stringify(message));
  return true;
}

function getStatusSnapshot() {
  return {
    connected: isSocketConnected(),
    connecting: isConnecting,
    wsState: currentSocketState(),
    serverUrl: ws?.url || configuredServerUrl,
    configuredServerUrl,
    clientId,
    reconnectAttempts,
    lastConnectedAt,
    lastDisconnectedAt,
    lastMessageAt,
    lastHeartbeatAt,
    lastError,
    tabCount: cachedTabs.length,
    tabsUpdatedAt,
  };
}

async function connect() {
  if (connectInFlight) {
    return connectInFlight;
  }

  connectInFlight = (async () => {
    reconnectGeneration += 1;
    const generation = reconnectGeneration;

    clearReconnectTimer();
    stopRuntimeLoops();

    if (ws) {
      try {
        ws.onclose = null;
        ws.close();
      } catch (err) {
        console.warn("Failed to close existing websocket", err);
      }
      ws = null;
    }

    isConnecting = true;
    lastError = null;

    const serverUrl = await loadConfiguredServerUrl();

    try {
      ws = new WebSocket(serverUrl);
    } catch (err) {
      isConnecting = false;
      lastError = formatError(err);
      reconnectAttempts += 1;
      scheduleReconnect();
      throw err;
    }

    ws.onopen = async () => {
      if (!ws || generation !== reconnectGeneration) {
        return;
      }

      isConnecting = false;
      reconnectAttempts = 0;
      lastConnectedAt = nowIso();
      lastError = null;

      startHeartbeatLoop();
      startTabsSyncLoop();

      sendMessage({
        type: "hello",
        clientId,
        userAgent: navigator.userAgent,
        extensionVersion: chrome.runtime.getManifest().version,
      });

      await emitTabsChanged();
    };

    ws.onmessage = async (event) => {
      lastMessageAt = nowIso();

      try {
        const payload = JSON.parse(event.data);
        await handleInboundMessage(payload);
      } catch (err) {
        lastError = formatError(err);
        console.error("Inbound message handling failed", err);
      }
    };

    ws.onerror = (event) => {
      const message = event?.message || "WebSocket error";
      lastError = formatError(message);
      console.error("WebSocket error", event);
    };

    ws.onclose = () => {
      if (generation !== reconnectGeneration) {
        return;
      }

      stopRuntimeLoops();
      isConnecting = false;
      lastDisconnectedAt = nowIso();
      ws = null;
      reconnectAttempts += 1;
      scheduleReconnect();
    };
  })()
    .catch((err) => {
      lastError = formatError(err);
      throw err;
    })
    .finally(() => {
      connectInFlight = null;
    });

  return connectInFlight;
}

async function handleInboundMessage(message) {
  if (!message || typeof message !== "object") {
    return;
  }

  if (message.type === "event" && message.event === "heartbeatAck") {
    return;
  }

  if (message.type !== "request") {
    return;
  }

  const { id, method, params } = message;

  if (!id || typeof method !== "string") {
    return;
  }

  try {
    const result = await dispatchRequest(method, params || {});
    sendMessage({ type: "response", id, ok: true, result });
  } catch (err) {
    sendMessage({
      type: "response",
      id,
      ok: false,
      error: err instanceof Error ? err.message : String(err),
    });
  }
}

async function dispatchRequest(method, params) {
  switch (method) {
    case "ping":
      return { pong: true, at: nowIso() };
    case "listTabs":
      return { tabs: await listTabs() };
    case "navigate":
      return await navigate(params);
    case "activateTab":
      return await activateTab(params);
    case "click":
      return await clickSelector(params);
    case "fill":
      return await fillSelector(params);
    case "press":
      return await pressSelector(params);
    case "evaluate":
      return await evaluateScript(params);
    case "extractText":
      return await extractText(params);
    case "waitFor":
      return await waitForCondition(params);
    case "captureScreenshot":
      return await captureScreenshot(params);
    case "extractPageContext":
      return await extractPageContext(params);
    case "getPageHtml":
      return await getPageHtml(params);
    default:
      throw new Error(`Unsupported method: ${method}`);
  }
}

function normalizeTabId(rawTabId) {
  const tabId = Number(rawTabId);
  if (!Number.isInteger(tabId) || tabId < 0) {
    throw new Error("tabId must be a non-negative integer");
  }
  return tabId;
}

function normalizeTimeout(rawTimeoutMs) {
  const timeoutMs = Number(rawTimeoutMs ?? 10_000);
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
    throw new Error("timeoutMs must be a positive number");
  }
  return Math.min(timeoutMs, 120_000);
}

function normalizeTab(tab) {
  return {
    id: tab.id,
    windowId: tab.windowId,
    active: tab.active,
    title: tab.title,
    url: tab.url,
    status: tab.status,
    audible: tab.audible,
    pinned: tab.pinned,
    incognito: tab.incognito,
  };
}

async function listTabs() {
  const tabs = await chrome.tabs.query({});
  const normalized = tabs.filter((tab) => typeof tab.id === "number").map(normalizeTab);
  cachedTabs = normalized;
  tabsUpdatedAt = nowIso();
  return normalized;
}

async function navigate(params) {
  const tabId = normalizeTabId(params.tabId);
  const url = String(params.url || "").trim();

  if (!url) {
    throw new Error("url is required");
  }

  const tab = await chrome.tabs.update(tabId, { url });
  return { tab: normalizeTab(tab) };
}

async function activateTab(params) {
  const tabId = normalizeTabId(params.tabId);
  const tab = await chrome.tabs.get(tabId);

  await chrome.tabs.update(tabId, { active: true });
  await chrome.windows.update(tab.windowId, { focused: true });

  const updatedTab = await chrome.tabs.get(tabId);
  return { tab: normalizeTab(updatedTab) };
}

async function clickSelector(params) {
  const tabId = normalizeTabId(params.tabId);
  const selector = String(params.selector || "").trim();
  const timeoutMs = normalizeTimeout(params.timeoutMs);

  if (!selector) {
    throw new Error("selector is required");
  }

  const [execution] = await chrome.scripting.executeScript({
    target: { tabId },
    args: [{ selector, timeoutMs }],
    func: async ({ selector, timeoutMs }) => {
      function waitForSelector(selector, timeoutMs) {
        const start = Date.now();

        return new Promise((resolve, reject) => {
          const check = () => {
            const element = document.querySelector(selector);
            if (element) {
              resolve(element);
              return;
            }

            if (Date.now() - start >= timeoutMs) {
              reject(new Error(`Timed out waiting for selector: ${selector}`));
              return;
            }

            setTimeout(check, 100);
          };

          check();
        });
      }

      const element = await waitForSelector(selector, timeoutMs);
      element.scrollIntoView({ behavior: "auto", block: "center", inline: "center" });
      element.dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));
      element.click();

      return { clicked: true, selector };
    },
  });

  return execution.result;
}

async function fillSelector(params) {
  const tabId = normalizeTabId(params.tabId);
  const selector = String(params.selector || "").trim();
  const value = String(params.value ?? "");
  const timeoutMs = normalizeTimeout(params.timeoutMs);

  if (!selector) {
    throw new Error("selector is required");
  }

  const [execution] = await chrome.scripting.executeScript({
    target: { tabId },
    args: [{ selector, value, timeoutMs }],
    func: async ({ selector, value, timeoutMs }) => {
      function waitForSelector(selector, timeoutMs) {
        const start = Date.now();

        return new Promise((resolve, reject) => {
          const check = () => {
            const element = document.querySelector(selector);
            if (element) {
              resolve(element);
              return;
            }

            if (Date.now() - start >= timeoutMs) {
              reject(new Error(`Timed out waiting for selector: ${selector}`));
              return;
            }

            setTimeout(check, 100);
          };

          check();
        });
      }

      const element = await waitForSelector(selector, timeoutMs);
      if (!(element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement || element instanceof HTMLSelectElement)) {
        throw new Error(`Element matched by '${selector}' is not a fillable input`);
      }

      element.focus();
      element.value = value;
      element.dispatchEvent(new Event("input", { bubbles: true }));
      element.dispatchEvent(new Event("change", { bubbles: true }));

      return { filled: true, selector, valueLength: value.length };
    },
  });

  return execution.result;
}

async function pressSelector(params) {
  const tabId = normalizeTabId(params.tabId);
  const selector = String(params.selector || "").trim();
  const key = String(params.key || "").trim();
  const timeoutMs = normalizeTimeout(params.timeoutMs);

  if (!selector) {
    throw new Error("selector is required");
  }

  if (!key) {
    throw new Error("key is required");
  }

  const [execution] = await chrome.scripting.executeScript({
    target: { tabId },
    args: [{ selector, key, timeoutMs }],
    func: async ({ selector, key, timeoutMs }) => {
      function waitForSelector(selector, timeoutMs) {
        const start = Date.now();

        return new Promise((resolve, reject) => {
          const check = () => {
            const element = document.querySelector(selector);
            if (element) {
              resolve(element);
              return;
            }

            if (Date.now() - start >= timeoutMs) {
              reject(new Error(`Timed out waiting for selector: ${selector}`));
              return;
            }

            setTimeout(check, 100);
          };

          check();
        });
      }

      const element = await waitForSelector(selector, timeoutMs);
      element.focus();
      element.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
      element.dispatchEvent(new KeyboardEvent("keyup", { key, bubbles: true }));

      if (key === "Enter" && (element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement)) {
        const form = element.form;
        if (form) {
          form.requestSubmit();
        }
      }

      return { pressed: true, selector, key };
    },
  });

  return execution.result;
}

async function evaluateScript(params) {
  const tabId = normalizeTabId(params.tabId);
  const script = String(params.script || "");
  const args = Array.isArray(params.args) ? params.args : [];

  if (!script.trim()) {
    throw new Error("script is required");
  }

  const [execution] = await chrome.scripting.executeScript({
    target: { tabId },
    args: [{ script, args }],
    func: ({ script, args }) => {
      const fn = new Function("args", script);
      return fn(args);
    },
  });

  return { value: execution.result };
}

async function extractText(params) {
  const tabId = normalizeTabId(params.tabId);
  const selector = params.selector ? String(params.selector) : "body";
  const maxLength = Math.min(Number(params.maxLength || 10_000), 100_000);

  const [execution] = await chrome.scripting.executeScript({
    target: { tabId },
    args: [{ selector, maxLength }],
    func: ({ selector, maxLength }) => {
      const node = document.querySelector(selector);
      if (!node) {
        throw new Error(`No element found for selector: ${selector}`);
      }

      const text = (node.innerText || node.textContent || "").trim();
      return {
        selector,
        text: text.slice(0, maxLength),
        truncated: text.length > maxLength,
      };
    },
  });

  return execution.result;
}

async function waitForCondition(params) {
  const tabId = normalizeTabId(params.tabId);
  const timeoutMs = normalizeTimeout(params.timeoutMs);
  const intervalMs = Math.max(50, Math.min(Number(params.intervalMs || 100), 2_000));
  const condition = String(params.condition || "element").trim();
  const selector = params.selector == null ? null : String(params.selector);
  const text = params.text == null ? null : String(params.text);
  const script = params.script == null ? null : String(params.script);

  const [execution] = await chrome.scripting.executeScript({
    target: { tabId },
    args: [{ condition, selector, text, script, timeoutMs, intervalMs }],
    func: async ({ condition, selector, text, script, timeoutMs, intervalMs }) => {
      const supportedConditions = new Set(["element", "text", "url", "function"]);
      if (!supportedConditions.has(condition)) {
        throw new Error(
          `Unsupported condition '${condition}'. Expected one of: element, text, url, function`
        );
      }

      const startedAt = Date.now();
      let dynamicCheck = null;
      if (condition === "function") {
        if (!script || !script.trim()) {
          throw new Error("script is required when condition is 'function'");
        }
        dynamicCheck = new Function("selector", "text", script);
      }

      const evaluate = () => {
        switch (condition) {
          case "element":
            if (!selector || !selector.trim()) {
              throw new Error("selector is required for condition 'element'");
            }
            return Boolean(document.querySelector(selector));
          case "text":
            if (!text) {
              throw new Error("text is required for condition 'text'");
            }
            return (document.body?.innerText || "").includes(text);
          case "url":
            if (!text) {
              throw new Error("text is required for condition 'url'");
            }
            return window.location.href.includes(text);
          case "function":
            return Boolean(dynamicCheck(selector, text));
          default:
            return false;
        }
      };

      while (Date.now() - startedAt < timeoutMs) {
        if (evaluate()) {
          return {
            ok: true,
            condition,
            selector,
            text,
            elapsedMs: Date.now() - startedAt,
            matchedAtUrl: window.location.href,
          };
        }
        await new Promise((resolve) => setTimeout(resolve, intervalMs));
      }

      throw new Error(
        `Timed out after ${timeoutMs}ms waiting for condition '${condition}'`
      );
    },
  });

  return execution.result;
}

async function captureScreenshot(params) {
  const tabId = normalizeTabId(params.tabId);
  const includeDataUrl = params.includeDataUrl !== false;
  const format = String(params.format || "png").toLowerCase();
  const quality = Math.max(1, Math.min(Number(params.quality || 80), 100));

  if (format !== "png" && format !== "jpeg") {
    throw new Error("format must be 'png' or 'jpeg'");
  }

  const tab = await chrome.tabs.get(tabId);
  if (!tab) {
    throw new Error(`Tab ${tabId} not found`);
  }

  await chrome.tabs.update(tabId, { active: true });
  await chrome.windows.update(tab.windowId, { focused: true });
  await new Promise((resolve) => setTimeout(resolve, 120));

  const options = format === "jpeg" ? { format, quality } : { format };
  const dataUrl = await chrome.tabs.captureVisibleTab(tab.windowId, options);
  const base64Payload = typeof dataUrl === "string" ? dataUrl.split(",")[1] || "" : "";
  const approxBytes = Math.floor((base64Payload.length * 3) / 4);

  return {
    tabId,
    windowId: tab.windowId,
    format,
    quality: format === "jpeg" ? quality : undefined,
    capturedAt: nowIso(),
    approximateBytes: approxBytes,
    dataUrl: includeDataUrl ? dataUrl : undefined,
  };
}

async function extractPageContext(params) {
  const tabId = normalizeTabId(params.tabId);
  const contextType = String(params.contextType || "all").toLowerCase();
  const maxElements = Math.max(1, Math.min(Number(params.maxElements || 50), 500));

  const [execution] = await chrome.scripting.executeScript({
    target: { tabId },
    args: [{ contextType, maxElements }],
    func: ({ contextType, maxElements }) => {
      const clipText = (value, maxLen = 300) =>
        String(value || "")
          .trim()
          .replace(/\s+/g, " ")
          .slice(0, maxLen);

      const allLinks = Array.from(document.querySelectorAll("a[href]"));
      const links = allLinks.slice(0, maxElements).map((node) => ({
        text: clipText(node.innerText || node.textContent, 200),
        href: node.href,
        title: clipText(node.title, 120),
      }));

      const allButtons = Array.from(
        document.querySelectorAll("button, input[type='button'], input[type='submit'], [role='button']")
      );
      const buttons = allButtons.slice(0, maxElements).map((node) => ({
        text: clipText(node.innerText || node.value || node.textContent, 200),
        id: node.id || null,
        className: clipText(node.className || "", 200),
        disabled: Boolean(node.disabled),
      }));

      const allInputs = Array.from(document.querySelectorAll("input, textarea, select"));
      const inputs = allInputs.slice(0, maxElements).map((node) => ({
        tag: node.tagName.toLowerCase(),
        type: node.type || null,
        name: node.name || null,
        id: node.id || null,
        placeholder: clipText(node.placeholder || "", 120),
        required: Boolean(node.required),
        disabled: Boolean(node.disabled),
      }));

      const allForms = Array.from(document.forms || []);
      const forms = allForms.slice(0, maxElements).map((form) => ({
        id: form.id || null,
        name: form.name || null,
        method: (form.method || "get").toLowerCase(),
        action: form.action || null,
        elementCount: form.elements.length,
      }));

      const metaTags = Array.from(document.querySelectorAll("meta"))
        .slice(0, maxElements)
        .map((meta) => ({
          name: meta.getAttribute("name"),
          property: meta.getAttribute("property"),
          content: clipText(meta.getAttribute("content") || "", 300),
        }));

      const payload = {
        title: document.title,
        url: window.location.href,
        summary: {
          links: allLinks.length,
          buttons: allButtons.length,
          inputs: allInputs.length,
          forms: allForms.length,
          metaTags: metaTags.length,
        },
        links,
        buttons,
        inputs,
        forms,
        metadata: metaTags,
      };

      switch (contextType) {
        case "links":
          return { title: payload.title, url: payload.url, summary: payload.summary, links };
        case "buttons":
          return { title: payload.title, url: payload.url, summary: payload.summary, buttons };
        case "inputs":
          return { title: payload.title, url: payload.url, summary: payload.summary, inputs };
        case "forms":
          return { title: payload.title, url: payload.url, summary: payload.summary, forms };
        case "metadata":
          return {
            title: payload.title,
            url: payload.url,
            summary: payload.summary,
            metadata: payload.metadata,
          };
        case "all":
          return payload;
        default:
          throw new Error(
            `Unsupported contextType '${contextType}'. Expected one of: all, links, buttons, inputs, forms, metadata`
          );
      }
    },
  });

  return execution.result;
}

async function getPageHtml(params) {
  const tabId = normalizeTabId(params.tabId);
  const selector = params.selector == null ? "html" : String(params.selector).trim();
  const maxLength = Math.max(1, Math.min(Number(params.maxLength || 300_000), 2_000_000));
  const stripScripts = params.stripScripts === true;
  const stripStyles = params.stripStyles === true;

  if (!selector) {
    throw new Error("selector cannot be empty");
  }

  const [execution] = await chrome.scripting.executeScript({
    target: { tabId },
    args: [{ selector, maxLength, stripScripts, stripStyles }],
    func: ({ selector, maxLength, stripScripts, stripStyles }) => {
      const node = document.querySelector(selector);
      if (!node) {
        throw new Error(`No element found for selector: ${selector}`);
      }

      let html = "";
      if (stripScripts || stripStyles) {
        const clone = node.cloneNode(true);
        if (stripScripts) {
          clone.querySelectorAll("script").forEach((el) => el.remove());
        }
        if (stripStyles) {
          clone
            .querySelectorAll("style, link[rel='stylesheet']")
            .forEach((el) => el.remove());
        }
        html = clone.outerHTML || "";
      } else {
        html = node.outerHTML || "";
      }

      const totalLength = html.length;
      const truncated = totalLength > maxLength;

      return {
        selector,
        html: html.slice(0, maxLength),
        truncated,
        totalLength,
        maxLength,
        title: document.title,
        url: window.location.href,
      };
    },
  });

  return execution.result;
}

function scheduleTabsChangedEvent() {
  if (tabsChangedTimer) {
    clearTimeout(tabsChangedTimer);
  }

  tabsChangedTimer = setTimeout(() => {
    tabsChangedTimer = null;
    emitTabsChanged().catch((err) => {
      console.warn("Failed to emit tabs changed", err);
    });
  }, 250);
}

async function emitTabsChanged() {
  const tabs = await listTabs();
  sendMessage({ type: "event", event: "tabsChanged", data: { tabs, at: nowIso() } });
}

chrome.storage.onChanged.addListener((changes, areaName) => {
  if (areaName !== "local") {
    return;
  }

  if (changes.serverUrl) {
    loadConfiguredServerUrl()
      .then(() => connect())
      .catch((err) => {
        lastError = formatError(err);
        console.error("Failed to reconnect after URL change", err);
      });
  }
});

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (message && message.type === "bridge:reconnect") {
    connect()
      .then(() => sendResponse({ ok: true, status: getStatusSnapshot() }))
      .catch((err) => sendResponse({ ok: false, error: formatError(err) }));
    return true;
  }

  if (message && message.type === "bridge:status") {
    sendResponse(getStatusSnapshot());
    return;
  }

  if (message && message.type === "bridge:syncTabs") {
    emitTabsChanged()
      .then(() => sendResponse({ ok: true, tabCount: cachedTabs.length }))
      .catch((err) => sendResponse({ ok: false, error: formatError(err) }));
    return true;
  }
});

chrome.tabs.onCreated.addListener(scheduleTabsChangedEvent);
chrome.tabs.onRemoved.addListener(scheduleTabsChangedEvent);
chrome.tabs.onUpdated.addListener(scheduleTabsChangedEvent);
chrome.tabs.onActivated.addListener(scheduleTabsChangedEvent);

init().catch((err) => {
  lastError = formatError(err);
  console.error("Failed to initialize service worker", err);
  reconnectAttempts += 1;
  scheduleReconnect();
});
