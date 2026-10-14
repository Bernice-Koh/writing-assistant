import { getCurrentWindow } from "@tauri-apps/api/window";

import { Overlay } from "./components/overlay";

export function App() {
  if (getCurrentWindow().label === "overlay") {
    return <Overlay />;
  }

  return (
    <main>
      <h1>Writing Assistant</h1>
    </main>
  );
}
