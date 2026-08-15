// Office.onReady resolving is what separates a task pane Word actually initialised from a
// page that merely rendered in the pane frame.
//
// Registers both document-change event tiers Office.js exposes for Word, side by side, so
// issue #13's spike can compare how often each actually fires against real typing: the
// cross-host Common API's DocumentSelectionChanged, and the Word-specific
// onParagraphChanged (WordApi 1.6+), which carries paragraph-level granularity and a
// local-versus-remote-coauthor source. The two read/write buttons exist to prove the document
// object model round-trips deliberately, the same way the browser extension spike used a
// keyboard trigger rather than relying on incidental typing alone.

const counts = { selection: 0, paragraph: 0 };

function log(kind, message) {
  const list = document.getElementById("log");
  if (!list) {
    return;
  }
  const entry = document.createElement("li");
  const timestamp = new Date().toISOString().slice(11, 23);
  entry.textContent = `${timestamp} [${kind}] ${message}`;
  list.prepend(entry);
}

function onSelectionChanged() {
  counts.selection += 1;
  log("selection-changed", `#${counts.selection}`);
}

function onParagraphChanged(event) {
  counts.paragraph += 1;
  log("paragraph-changed", `#${counts.paragraph} source=${event.source}`);
}

async function registerParagraphChanged() {
  if (!Office.context.requirements.isSetSupported("WordApi", "1.6")) {
    log("paragraph-changed", "WordApi 1.6 not supported on this Word build, skipped.");
    return;
  }
  await Word.run(async (context) => {
    context.document.onParagraphChanged.add(onParagraphChanged);
    await context.sync();
  });
}

async function readParagraph() {
  await Word.run(async (context) => {
    const paragraphs = context.document.getSelection().paragraphs;
    paragraphs.load("text");
    await context.sync();
    const text = paragraphs.items.map((paragraph) => paragraph.text).join("\n");
    log("read", text.length > 0 ? text : "(empty selection)");
  });
}

async function insertTestText() {
  await Word.run(async (context) => {
    const marker = `[word-addin-spike ${new Date().toISOString()}]`;
    context.document.getSelection().insertText(marker, Word.InsertLocation.replace);
    await context.sync();
    log("write", `inserted "${marker}"`);
  });
}

Office.onReady((info) => {
  const status = document.getElementById("status");
  if (status) {
    status.textContent = `Office ready. Host: ${info.host}, platform: ${info.platform}.`;
  }

  Office.context.document.addHandlerAsync(
    Office.EventType.DocumentSelectionChanged,
    onSelectionChanged,
  );
  registerParagraphChanged().catch((error) =>
    log("paragraph-changed", `registration failed: ${error.message}`),
  );

  document.getElementById("read-paragraph")?.addEventListener("click", () => {
    readParagraph().catch((error) => log("read", `failed: ${error.message}`));
  });
  document.getElementById("insert-test-text")?.addEventListener("click", () => {
    insertTestText().catch((error) => log("write", `failed: ${error.message}`));
  });
});
