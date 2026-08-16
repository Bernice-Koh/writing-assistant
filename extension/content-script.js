// Content script for the web capture bridge (issue #21). Tracks the focused editable element as
// focus moves around the page, and answers the background script's request-text and replace
// messages against whichever element is currently tracked, so the Rust core can pull text and
// push a replacement on its own schedule rather than on a manual trigger.

console.debug("[writing-assistant] content script injected on", location.href);

let focusedTarget = null;

function focusedEditable(element) {
  if (element.tagName === "TEXTAREA" || element.tagName === "INPUT") {
    return { element, kind: "value" };
  }
  if (element.isContentEditable) {
    return { element, kind: "content-editable" };
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

// Capture phase, not bubble: a site's own focus handler further down the tree can stop this
// event from ever reaching a bubble-phase listener on `document`, the same reasoning the
// removed keydown-trigger spike (#12) applied to its own listener.
document.addEventListener(
  "focusin",
  (event) => {
    focusedTarget = focusedEditable(event.target);
  },
  true,
);

document.addEventListener(
  "focusout",
  (event) => {
    if (focusedTarget && event.target === focusedTarget.element) {
      focusedTarget = null;
    }
  },
  true,
);

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (message.type === "request-text") {
    sendResponse(focusedTarget ? { text: readText(focusedTarget) } : { noFocus: true });
    return;
  }
  if (message.type === "replace") {
    if (!focusedTarget) {
      sendResponse({ found: false });
      return;
    }
    const text = readText(focusedTarget);
    const anchorIndex = text.indexOf(message.anchor);
    if (anchorIndex === -1) {
      sendResponse({ found: false });
      return;
    }
    const start = anchorIndex + message.localStart;
    const end = start + message.localLength;
    writeText(focusedTarget, text.slice(0, start) + message.replacement + text.slice(end));
    sendResponse({ found: true });
  }
});
