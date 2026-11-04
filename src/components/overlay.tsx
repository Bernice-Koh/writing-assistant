import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface Span {
  anchor: string;
  localStart: number;
  localLength: number;
  occurrence: number;
}

type FlagOrigin = "spelling" | "grammar" | "style" | "aiTell";

interface ClauseCitation {
  clauseId: number;
  ruleId: string;
  clauseText: string;
}

interface Flag {
  id: string;
  origin: FlagOrigin;
  /** Present on a style flag only: the engine holds the citation in the style origin itself. */
  citation?: ClauseCitation;
  /** Present on a telltale only: the engine holds the pattern ids in the telltale origin itself. */
  patternIds?: string[];
  span: Span;
  message: string;
  suggestions: string[];
  sourceDetail: string;
  rulesetVersion: number | null;
  catalogueHash: string | null;
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
  style: `${UNDERLINE_THICKNESS}px dashed #d97706`,
  aiTell: `${UNDERLINE_THICKNESS}px dotted #a855f7`,
};

/**
 * Renders one underline per {@link CursorRect} the backend already resolved for each flag, and a
 * card with the flag's message, when it has one, and suggestions, under the cited clause's text on
 * a style flag, while the native hover loop (`overlay::track_hover` in `src-tauri/src/overlay.rs`)
 * reports the cursor sitting over one or over its card. Coordinates arrive in CSS pixels, already
 * relative to this window's own top-left: the overlay window is itself positioned at the document
 * view's origin, and `overlay::relative_flags` divides UI Automation's physical pixels down by the
 * window's scale factor. No translation and no unit conversion happens here, since React renders a
 * bare number in a style as a CSS pixel, the space these are already in.
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

/**
 * The card for a hovered flag. Its rendered rectangle is reported to the backend after every layout
 * that can move or resize it, and the native hover loop keeps the hover while the pointer is inside
 * that rectangle, so the pointer can move from the underline onto the card. The gap between the
 * underline and the card is padding of the reported element, so the reported rectangle meets the
 * underline's own and a pointer crossing the gap never leaves both.
 */
function FlagCard({ positioned }: { positioned: PositionedFlag }) {
  const cardRef = useRef<HTMLDivElement>(null);
  const [placement, setPlacement] = useState<"below" | "above">("below");
  const rect = positioned.rects[0];
  const flagId = positioned.flag.id;

  useLayoutEffect(() => {
    const card = cardRef.current;
    if (!card) {
      return;
    }
    const overflowsBelow = rect.y + rect.height + card.offsetHeight > window.innerHeight;
    setPlacement(overflowsBelow ? "above" : "below");
  }, [rect.y, rect.height]);

  useLayoutEffect(() => {
    const card = cardRef.current;
    if (!card) {
      return;
    }
    const bounds = card.getBoundingClientRect();
    invoke("set_card_rect", {
      flagId,
      rect: { x: bounds.left, y: bounds.top, width: bounds.width, height: bounds.height },
    }).catch((error: unknown) => {
      console.error("could not report the card's rectangle", error);
    });
  }, [flagId, rect.x, rect.y, rect.height, placement]);

  return (
    <div
      ref={cardRef}
      style={{
        position: "absolute",
        left: rect.x,
        top: placement === "below" ? rect.y + rect.height : rect.y,
        transform: placement === "above" ? "translateY(-100%)" : undefined,
        paddingTop: placement === "below" ? CARD_GAP : 0,
        paddingBottom: placement === "above" ? CARD_GAP : 0,
      }}
    >
      <div
        style={{
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
        {positioned.flag.citation && (
          <div style={{ marginBottom: 4, fontStyle: "italic", color: "rgba(255, 255, 255, 0.75)" }}>
            {positioned.flag.citation.clauseText}
          </div>
        )}
        {positioned.flag.message !== "" && <div>{positioned.flag.message}</div>}
        {positioned.flag.suggestions.length > 0 && (
          <ul style={{ margin: "4px 0 0", paddingLeft: 16 }}>
            {positioned.flag.suggestions.map((suggestion) => (
              <li key={suggestion}>{suggestion}</li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
