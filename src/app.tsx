import { getCurrentWindow } from "@tauri-apps/api/window";

import { Overlay } from "./components/overlay";
import { useShellStore } from "./stores/shell-store";

export function App() {
  const isCheckingEnabled = useShellStore((state) => state.isCheckingEnabled);
  const setCheckingEnabled = useShellStore((state) => state.setCheckingEnabled);

  if (getCurrentWindow().label === "overlay") {
    return <Overlay />;
  }

  return (
    <main>
      <h1>Writing Assistant</h1>
      <p>Live checking: {isCheckingEnabled ? "on" : "off"}</p>
      <button type="button" onClick={() => setCheckingEnabled(!isCheckingEnabled)}>
        Toggle live checking
      </button>
    </main>
  );
}
