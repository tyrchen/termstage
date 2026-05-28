import type { TerminalViewportOrigin } from './socket';

const VIEWPORT_ORIGIN_MAX = 10000;
const WHEEL_PIXEL_CELL = 36;
const WHEEL_LINE_CELL = 3;
const WHEEL_PAGE_CELL = 24;

export function watchBackendViewportNavigation(
  root: HTMLElement,
  sendViewport: (origin: TerminalViewportOrigin) => void
): () => void {
  let originCol = 0;

  const onWheel = (event: WheelEvent): void => {
    const horizontalDelta = horizontalViewportDelta(event);
    if (horizontalDelta === 0) {
      return;
    }
    const nextOriginCol = clampViewportOrigin(originCol + horizontalDelta);
    if (nextOriginCol === originCol) {
      return;
    }
    originCol = nextOriginCol;
    event.preventDefault();
    event.stopPropagation();
    sendViewport({ col: originCol });
  };

  root.addEventListener('wheel', onWheel, { capture: true, passive: false });
  return () => {
    root.removeEventListener('wheel', onWheel, { capture: true });
  };
}

function horizontalViewportDelta(event: WheelEvent): number {
  const rawDelta = event.deltaX !== 0 ? event.deltaX : event.shiftKey ? event.deltaY : 0;
  if (rawDelta === 0) {
    return 0;
  }
  const direction = Math.sign(rawDelta);
  const magnitude = Math.abs(rawDelta);
  if (event.deltaMode === WheelEvent.DOM_DELTA_PAGE) {
    return direction * WHEEL_PAGE_CELL;
  }
  if (event.deltaMode === WheelEvent.DOM_DELTA_LINE) {
    return direction * Math.max(1, Math.round(magnitude * WHEEL_LINE_CELL));
  }
  return direction * Math.max(1, Math.round(magnitude / WHEEL_PIXEL_CELL));
}

function clampViewportOrigin(value: number): number {
  return Math.min(VIEWPORT_ORIGIN_MAX, Math.max(0, value));
}
