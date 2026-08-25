import { damp } from "./easing";

export interface GraphPaintState {
  hoverStrength: number;
  selectionStrength: number;
  displayHoverId: string | null;
}

export const INITIAL_GRAPH_PAINT: GraphPaintState = {
  hoverStrength: 0,
  selectionStrength: 1,
  displayHoverId: null,
};

const SETTLE = 0.01;

export function stepGraphPaint(
  state: GraphPaintState,
  input: {
    hoveredCommitId: string | null;
    selectionReset: boolean;
    deltaMs: number;
    reducedMotion: boolean;
  },
): { next: GraphPaintState; animating: boolean } {
  const hoverTarget = input.hoveredCommitId ? 1 : 0;
  let hoverStrength = state.hoverStrength;
  let selectionStrength = input.selectionReset ? 0 : state.selectionStrength;
  let displayHoverId = input.hoveredCommitId ?? state.displayHoverId;

  if (input.reducedMotion) {
    hoverStrength = hoverTarget;
    selectionStrength = 1;
    displayHoverId = input.hoveredCommitId;
  } else {
    hoverStrength = damp(hoverStrength, hoverTarget, input.deltaMs, 70);
    selectionStrength = damp(selectionStrength, 1, input.deltaMs, 90);
  }

  if (Math.abs(hoverStrength - hoverTarget) < SETTLE) hoverStrength = hoverTarget;
  if (Math.abs(selectionStrength - 1) < SETTLE) selectionStrength = 1;

  if (hoverTarget === 0 && hoverStrength === 0) {
    displayHoverId = null;
  }

  const animating = hoverStrength !== hoverTarget || selectionStrength !== 1;

  return {
    next: { hoverStrength, selectionStrength, displayHoverId },
    animating,
  };
}
