// Service worker for the web capture bridge spike (issue #12). Owns the one WebSocket
// connection to the local Rust core and relays capture and echo messages between it and
// content scripts.
//
// A live WebSocket keeps this worker alive past Chrome's idle timer as of Chrome 116, which is
// why the manifest pins that as the minimum version. The heartbeat alarm below covers the gap
// between real typing activity, since nothing would otherwise touch the socket for over 30s.

const BRIDGE_URL = "ws://127.0.0.1:47826";
const HEARTBEAT_MARKER = "__writing_assistant_heartbeat__";
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

function handleServerMessage(event) {
  const message = JSON.parse(event.data);
  if (message.type !== "echo" || message.text.includes(HEARTBEAT_MARKER)) {
    return;
  }
  chrome.tabs.query({ active: true, lastFocusedWindow: true }, (tabs) => {
    const tab = tabs[0];
    if (tab?.id !== undefined) {
      chrome.tabs.sendMessage(tab.id, { type: "capture-reply", text: message.text });
    }
  });
}

chrome.runtime.onMessage.addListener((message) => {
  if (message.type !== "capture-request") {
    return;
  }
  connect();
  if (socket?.readyState === WebSocket.OPEN) {
    socket.send(JSON.stringify({ type: "capture", text: message.text }));
  }
});

chrome.alarms.create(HEARTBEAT_ALARM, { periodInMinutes: HEARTBEAT_PERIOD_MINUTES });
chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name !== HEARTBEAT_ALARM) {
    return;
  }
  connect();
  if (socket?.readyState === WebSocket.OPEN) {
    socket.send(JSON.stringify({ type: "capture", text: HEARTBEAT_MARKER }));
  }
});

chrome.runtime.onStartup.addListener(connect);
connect();
