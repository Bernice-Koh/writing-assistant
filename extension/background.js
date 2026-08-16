// Service worker for the web capture bridge (issue #21). Owns the one WebSocket connection to
// the local Rust core, relays its request_text/replace requests to the focused tab's content
// script, and answers back over the socket with whatever the content script replies.
//
// A live WebSocket keeps this worker alive past Chrome's idle timer as of Chrome 116, which is
// why the manifest pins that as the minimum version. The heartbeat alarm below covers the gap
// between real typing activity, since nothing would otherwise touch the socket for over 30s.

const BRIDGE_URL = "ws://127.0.0.1:47826";
const HEARTBEAT_ALARM = "writing-assistant-keepalive";
const HEARTBEAT_PERIOD_MINUTES = 0.4;

let socket = null;

function connect() {
  if (
    socket &&
    (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CONNECTING)
  ) {
    return;
  }
  socket = new WebSocket(BRIDGE_URL);
  socket.addEventListener("message", handleServerMessage);
  socket.addEventListener("close", () => {
    socket = null;
  });
  socket.addEventListener("error", () => {
    socket = null;
  });
}

function send(message) {
  if (socket?.readyState === WebSocket.OPEN) {
    socket.send(JSON.stringify(message));
  }
}

async function activeTabId() {
  const [tab] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
  return tab?.id;
}

// No content script is listening when the active tab doesn't match its `matches` patterns, is a
// browser-internal page, or hasn't finished loading yet; `chrome.tabs.sendMessage` then rejects
// rather than resolving with a reply. Treated the same as no reply, so a request never hangs the
// core waiting for an answer that will never come.
async function relayToContentScript(tabId, message) {
  if (tabId === undefined) {
    return null;
  }
  try {
    return await chrome.tabs.sendMessage(tabId, message);
  } catch (error) {
    console.debug("[writing-assistant] content script did not respond:", error);
    return null;
  }
}

async function handleServerMessage(event) {
  const message = JSON.parse(event.data);
  const tabId = await activeTabId();

  if (message.type === "request_text") {
    const reply = await relayToContentScript(tabId, { type: "request-text" });
    if (reply?.noFocus || !reply) {
      send({ type: "no_focus" });
    } else {
      send({ type: "current_text", text: reply.text });
    }
    return;
  }

  if (message.type === "replace") {
    const reply = await relayToContentScript(tabId, {
      type: "replace",
      anchor: message.anchor,
      localStart: message.local_start,
      localLength: message.local_length,
      replacement: message.replacement,
    });
    send({ type: "replace_result", found: Boolean(reply?.found) });
  }
}

chrome.alarms.create(HEARTBEAT_ALARM, { periodInMinutes: HEARTBEAT_PERIOD_MINUTES });
chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name !== HEARTBEAT_ALARM) {
    return;
  }
  connect();
  send({ type: "heartbeat" });
});

chrome.runtime.onStartup.addListener(connect);
connect();
