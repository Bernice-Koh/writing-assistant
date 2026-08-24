import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface Span {
  anchor: string;
  localStart: number;
  localLength: number;
}

type FlagOrigin = "spelling" | "grammar" | "aiTell";

interface Flag {
  id: string;
  origin: FlagOrigin;
  span: Span;
  message: string;
  suggestions: string[];
  sourceDetail: string;
}

interface CursorRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

interface PositionedFlag {
  flag: Flag;
  rects: CursorRect[];
}

const UNDERLINE_THICKNESS = 2;

const UNDERLINE_BORDER: Record<FlagOrigin, string> = {
  spelling: `${UNDERLINE_THICKNESS}px solid #e5484d`,
  grammar: `${UNDERLINE_THICKNESS}px solid #3b82f6`,
  aiTell: `${UNDERLINE_THICKNESS}px dotted #a855f7`,
};

/**
 * Renders one underline per {@link CursorRect} the backend already resolved for each flag, and a
 * card with the flag's message and suggestions while the native hover loop (`overlay::track_hover`
 * in `src-tauri/src/overlay.rs`) reports the cursor sitting over one. Coordinates arrive already
 * relative to this window's own top-left, since the overlay window is itself positioned at the
 * document view's origin; no further translation happens here.
 */
export function Overlay() {
  const [flags, setFlags] = useState<PositionedFlag[]>([]);
  const [hoveredId, setHoveredId] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    invoke<PositionedFlag[]>("get_current_flags")
      .then((initial) => {
        if (!cancelled) {
          setFlags(initial);
        }
      })
      .catch((error: unknown) => {
        console.error("could not load the initial flag set", error);
      });

    const unlistenFlags = listen<PositionedFlag[]>("flags-updated", (event) => {
      setFlags(event.payload);
    });
    const unlistenHovered = listen<string>("flag-hovered", (event) => {
      setHoveredId(event.payload);
    });
    const unlistenUnhovered = listen("flag-unhovered", () => {
      setHoveredId(null);
    });

    return () => {
      cancelled = true;
      unlistenFlags.then((unlisten) => unlisten());
      unlistenHovered.then((unlisten) => unlisten());
      unlistenUnhovered.then((unlisten) => unlisten());
    };
  }, []);

  const hovered = flags.find((positioned) => positioned.flag.id === hoveredId);

  return (
    <div style={{ position: "relative", width: "100vw", height: "100vh" }}>
      {flags.flatMap((positioned) =>
        positioned.rects.map((rect, index) => (
          <div
            key={`${positioned.flag.id}-${index}`}
            style={{
              position: "absolute",
              left: rect.x,
              top: rect.y + rect.height - UNDERLINE_THICKNESS,
              width: rect.width,
              height: UNDERLINE_THICKNESS,
              borderBottom: UNDERLINE_BORDER[positioned.flag.origin],
            }}
          />
        )),
      )}
      {hovered && <FlagCard positioned={hovered} />}
    </div>
  );
}

/** How far the card sits from the rect it explains, on whichever side it lands. */
const CARD_GAP = 6;

function FlagCard({ positioned }: { positioned: PositionedFlag }) {
  const cardRef = useRef<HTMLDivElement>(null);
  const [placement, setPlacement] = useState<"below" | "above">("below");
  const rect = positioned.rects[0];

  useLayoutEffect(() => {
    const card = cardRef.current;
    if (!card) {
      return;
    }
    const overflowsBelow = rect.y + rect.height + CARD_GAP + card.offsetHeight > window.innerHeight;
    setPlacement(overflowsBelow ? "above" : "below");
  }, [rect.y, rect.height]);

  return (
    <div
      ref={cardRef}
      style={{
        position: "absolute",
        left: rect.x,
        top: placement === "below" ? rect.y + rect.height + CARD_GAP : rect.y - CARD_GAP,
        transform: placement === "above" ? "translateY(-100%)" : undefined,
        maxWidth: 320,
        padding: "8px 12px",
        borderRadius: 6,
        background: "rgba(30, 30, 30, 0.95)",
        color: "white",
        fontFamily: "sans-serif",
        fontSize: 13,
        lineHeight: 1.4,
        boxShadow: "0 2px 8px rgba(0, 0, 0, 0.35)",
      }}
    >
      <div>{positioned.flag.message}</div>
      {positioned.flag.suggestions.length > 0 && (
        <ul style={{ margin: "4px 0 0", paddingLeft: 16 }}>
          {positioned.flag.suggestions.map((suggestion) => (
            <li key={suggestion}>{suggestion}</li>
          ))}
        </ul>
      )}
    </div>
  );
}
