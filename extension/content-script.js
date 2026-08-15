// Content script for the web capture bridge spike (issue #12). Reads the focused editable
// element on a keyboard trigger, round-trips it through the background script to the Rust
// core, and writes the reply back into the same element. Proves DOM read, the messaging round
// trip, and DOM write against a real page; the trigger is temporary scaffolding for manual
// verification, not the eventual live-check flow.

const TRIGGER_KEY = "y";

console.debug("[writing-assistant] content script injected on", location.href);

function focusedEditable() {
  const active = document.activeElement;
  if (!active) {
    return null;
  }
  if (active.tagName === "TEXTAREA" || active.tagName === "INPUT") {
    return { element: active, kind: "value" };
  }
  if (active.isContentEditable) {
    return { element: active, kind: "content-editable" };
  }
  return null;
}

function readText(target) {
  return target.kind === "value" ? target.element.value : target.element.innerText;
}

function writeText(target, text) {
  if (target.kind === "value") {
    target.element.value = text;
  } else {
    target.element.innerText = text;
  }
  target.element.dispatchEvent(new Event("input", { bubbles: true }));
}

let pendingTarget = null;

// Capture phase, not bubble: a site's own keydown handler further down the tree can call
// stopPropagation before a bubble-phase listener on `document` ever sees the event, and Gmail's
// compose surface does exactly that for several shortcuts.
document.addEventListener(
  "keydown",
  (event) => {
    if (!event.ctrlKey || !event.shiftKey || event.key.toLowerCase() !== TRIGGER_KEY) {
      return;
    }
    console.debug("[writing-assistant] trigger seen, activeElement:", document.activeElement);
    const target = focusedEditable();
    if (!target) {
      console.debug("[writing-assistant] no editable target found");
      return;
    }
    event.preventDefault();
    pendingTarget = target;
    console.debug("[writing-assistant] sending capture-request", {
      length: readText(target).length,
    });
    chrome.runtime.sendMessage({ type: "capture-request", text: readText(target) });
  },
  true,
);

chrome.runtime.onMessage.addListener((message) => {
  if (message.type !== "capture-reply" || !pendingTarget) {
    return;
  }
  writeText(pendingTarget, message.text);
  pendingTarget = null;
});
