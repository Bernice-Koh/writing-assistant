import { create } from "zustand";

/**
 * Shell state that outlives any single component tree, which is what the tray, the overlay,
 * and settings all need. Per-view state stays local to its component.
 */
interface ShellState {
  isCheckingEnabled: boolean;
  setCheckingEnabled: (isEnabled: boolean) => void;
}

export const useShellStore = create<ShellState>((set) => ({
  isCheckingEnabled: true,
  setCheckingEnabled: (isEnabled) => set({ isCheckingEnabled: isEnabled }),
}));
