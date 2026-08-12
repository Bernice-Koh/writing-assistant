import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./app";

const container = document.getElementById("root");
if (!container) {
  throw new Error("index.html did not provide the #root element React mounts into");
}

createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
