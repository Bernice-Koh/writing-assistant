// Local HTTPS static file server for the Word add-in task pane, for issue #13's spike. Office
// Add-ins refuse to load a task pane over plain HTTP, even for local development, so this uses
// office-addin-dev-certs to generate and trust a localhost certificate rather than working
// around that requirement.

import { readFile } from "node:fs/promises";
import { createServer } from "node:https";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

import { getHttpsServerOptions } from "office-addin-dev-certs";

const PORT = 3000;
const ROOT = fileURLToPath(new URL(".", import.meta.url));

// Matches manifest.xml's SourceLocation and IconUrl entries, which are fixed at this port.
const CONTENT_TYPES = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".png": "image/png",
};

async function handleRequest(request, response) {
  // Word appends a query string to the task pane URL (host, platform, locale), so the path
  // must be parsed out rather than used as the request URL directly.
  const { pathname } = new URL(request.url, "http://localhost");
  const requestedPath = pathname === "/" ? "/taskpane.html" : pathname;
  const filePath = normalize(join(ROOT, requestedPath));
  if (!filePath.startsWith(ROOT)) {
    response.writeHead(403).end();
    return;
  }
  try {
    const body = await readFile(filePath);
    const contentType = CONTENT_TYPES[extname(filePath)] ?? "application/octet-stream";
    response.writeHead(200, { "Content-Type": contentType }).end(body);
  } catch {
    response.writeHead(404).end();
  }
}

const httpsOptions = await getHttpsServerOptions();
createServer(httpsOptions, (request, response) => {
  handleRequest(request, response).catch((error) => {
    console.error(`request for ${request.url} failed:`, error);
    response.writeHead(500).end();
  });
}).listen(PORT, () => {
  console.log(`Word add-in task pane served at https://localhost:${PORT}/taskpane.html`);
});
