---
name: notebook-layout-rendering
description: >
  Frontend rendering invariants for the notebook view: stable DOM order
  with CSS visual positioning, iframe lifecycle across reorders, scroll
  anchoring during output growth, layout pulses for isolated renderers,
  and ResizeObserver-based scroll pinning. Use when changing cell
  rendering, output display, scroll behavior, iframe isolation, or
  anything that affects visual stability during notebook mutations.
---

# Notebook Layout & Rendering Patterns

Use this skill when working on cell rendering order, output area layout,
iframe lifecycle, scroll behavior during execution, or visual stability.
These patterns exist because notebooks are uniquely challenging: cells
reorder, outputs grow asynchronously, iframes must survive DOM mutations,
and the user's scroll position must remain stable while content changes
around them.

## The Stable DOM Order Invariant

**The most important rendering rule in the codebase.**

`NotebookView.tsx` MUST render cells in stable DOM order (sorted by
cell ID) and use CSS `order` for visual positioning:

```tsx
// CORRECT: Stable DOM order with CSS visual positioning
const stableDomOrder = useMemo(() => [...cellIds].sort(), [cellIds]);

{stableDomOrder.map((cellId) => (
  <CellWrapper
    key={cellId}
    style={{ order: cellIdToIndex.get(cellId) }}
    ...
  />
))}
```

The parent container uses `display: flex; flex-direction: column` so
each child's `order` property controls visual position.

### Why This Exists

When React reconciles a reordered list, it calls `insertBefore()` on
DOM nodes that moved. For normal divs this is invisible — the browser
relocates the node. But for `<iframe>` elements, `insertBefore()` causes
the browser to **destroy and recreate** the iframe's browsing context:

- All JavaScript state is lost
- Widget instances are destroyed
- Rendered outputs flash white
- Theme state resets
- The iframe goes through the full ready → eval → render cycle again

By keeping DOM order stable (sorted by cell ID, which never changes),
React never calls `insertBefore()`. When the user moves a cell, only
the `style.order` prop changes — a CSS-only update that preserves the
iframe's browsing context.

### What Would Break This

- Using `cellIds.map()` directly instead of `stableDomOrder.map()`
- Changing the sort key (cell IDs are UUIDs, stable for the cell's
  lifetime)
- Adding a wrapper div that conditionally renders around cells
- Using CSS Grid with `grid-row` instead of flexbox `order`

## Iframe Lifecycle Management

### IsolatedFrame State Machine

Each iframe output goes through:

```
mount → blob URL created → iframe loads → "ready" message →
bootstrap JS fetched → eval sent → "renderer_ready" →
content rendered → resize messages
```

Key states tracked in `isolated-frame.tsx`:
- `isIframeReady` — iframe element loaded
- `isReady` — React renderer bundle initialized inside iframe
- `isContentRendered` — first content rendered (for reveal mode)
- `isReloading` — iframe is being recreated after DOM move

### Reload Detection

If despite the stable DOM order invariant an iframe does get reloaded
(e.g., a browser bug, or a parent component remounts), the system
detects it:

1. `hasReceivedReadyRef` tracks whether a "ready" message was already
   received
2. A subsequent "ready" from the same iframe means it reloaded
3. The iframe is hidden (`isReloading = true`) to prevent white flash
4. Full bootstrap sequence replays: eval → render → reveal

### Layout Pulses for Isolated Renderers

After rendering content inside an iframe, some outputs (especially
virtualized ones like Vega-Lite, Plotly, DataFrames) don't immediately
know their correct size. The `pulseRendererLayout()` function fires
synthetic events to trigger re-measurement:

```typescript
const LAYOUT_PULSE_DELAYS_MS = [0, 160, 600];

function pulseRendererLayout(): void {
  window.dispatchEvent(new Event("resize"));
  window.dispatchEvent(new Event("scroll"));
  document.dispatchEvent(new Event("scroll"));
  document.body?.dispatchEvent(new Event("scroll"));
  // Also sends height to parent
  window.parent.postMessage({
    type: "resize",
    payload: { height: document.body.scrollHeight },
  }, "*");
}
```

Pulses fire on three schedules (0ms, 160ms, 600ms) via
`requestAnimationFrame` to give outputs a settling window. This is
triggered after `render`, `batch_render`, and `clear` operations.

**When to add layout pulses:** If a new output type relies on
measuring its container size on mount (libraries that use
`ResizeObserver` internally, or that compute layout in
`requestAnimationFrame`), it likely needs the pulse mechanism.

## Scroll Stability During Output Growth

When a cell executes and produces output, the output area grows,
pushing content below it downward. If the user is editing a cell
below the executing one, their cursor jumps off-screen. Three
mechanisms work together to prevent this:

### 1. Native Scroll Anchoring

The notebook scroll container uses `overflow-anchor: auto` (the
browser default that was previously disabled with `none`). This tells
Chromium and Firefox to automatically adjust scroll position when
content above the viewport grows. WebKit ignores this property.

### 2. ResizeObserver Scroll Pin

For WebKit compatibility and more precise control, `useEditorRegistry`
implements a short-lived scroll pin:

1. When `focusCell()` is called, it scrolls the cell into view
2. Creates a `ResizeObserver` on the scroll content element
3. On each resize (output growth), calls `scrollIntoView({ block:
   "nearest", behavior: "auto" })` on the focused editor
4. The pin auto-expires after `SCROLL_PIN_DURATION_MS` (2500ms)
5. The pin cancels immediately on manual scroll (wheel, touch,
   PageDown/Home/End/Space keys)

This ensures the focused editor stays visible during the initial
burst of output from execution, without fighting the user's intentional
scrolling.

### 3. Output Area Anchor Opt-Out

Output areas are excluded from the browser's scroll anchoring
algorithm (via `overflow-anchor: none` on output containers) because
their size changes are the *cause* of scroll disruption, not something
to anchor against.

### The Interaction Model

```
User clicks "Run" on cell 3:
  1. focusCell("cell-3") called
  2. cell-3 scrolled into view
  3. ResizeObserver pin created on scroll content
  4. Cell 3 outputs start arriving (grows output area)
  5. Content below cell 3 pushes down
  6. ResizeObserver fires → scrollIntoView keeps editor visible
  7. If user scrolls manually → pin cancelled immediately
  8. After 2.5s → pin expires regardless
```

## Container CSS Properties

The notebook scroll container has specific CSS that enables these
patterns:

```css
.notebook-scroll-container {
  flex: 1;
  overflow-y: auto;
  overflow-x: clip;           /* prevent horizontal scroll */
  overscroll-x: contain;      /* don't chain to parent */
  contain: paint;             /* layout containment for perf */
  overflow-anchor: auto;      /* enable native scroll anchoring */
}
```

`contain: paint` is important for performance — it tells the browser
that painting inside this container doesn't affect anything outside it,
enabling optimizations for the many output iframes.

## Decision Framework

| Situation | Approach |
|-----------|----------|
| Adding a new cell wrapper | Must use `order` style prop, render in stable DOM order |
| New output type doesn't size correctly | Add layout pulse support in isolated renderer |
| Scroll jumps during execution | Check overflow-anchor, ResizeObserver pin, output area opt-out |
| Iframe flashes white on cell move | Verify stable DOM order; check reload detection |
| New interactive output needs resize events | Ensure pulseRendererLayout fires after content update |
| Output area causes scroll anchor jitter | Opt output container out with `overflow-anchor: none` |
| User's cursor jumps off-screen | Check ResizeObserver pin timing and cancellation logic |
| Adding drag-and-drop for cells | MUST NOT reorder DOM nodes — only change CSS `order` values |

## Key Source Files

| File | What it controls |
|------|-----------------|
| `apps/notebook/src/components/NotebookView.tsx` | Stable DOM order, `stableDomOrder`, `cellIdToIndex`, flexbox container |
| `apps/notebook/src/hooks/useEditorRegistry.tsx` | ResizeObserver scroll pin, `focusCell`, pin cancellation |
| `src/components/isolated/isolated-frame.tsx` | Iframe lifecycle, reload detection, `isReloading` state |
| `src/isolated-renderer/index.tsx` | Layout pulses (`pulseRendererLayout`, `scheduleRendererLayoutPulses`) |
| `src/components/cell/OutputArea.tsx` | Output area rendering, anchor opt-out |
| `apps/notebook/src/components/CellWrapper.tsx` | Per-cell `order` style, cell DOM structure |

## Common Mistakes

### 1. Iterating cellIds directly for rendering

```tsx
// WRONG: React will call insertBefore on reorder
{cellIds.map(id => <Cell key={id} ... />)}

// CORRECT: Stable DOM order + CSS visual position
{stableDomOrder.map(id => <Cell key={id} style={{ order: indexOf(id) }} />)}
```

### 2. Assuming iframes survive DOM moves

They don't. Any code that causes React to move an iframe's DOM node
in the tree (even within the same parent) will destroy and reload it.
Test cell reorder operations manually to verify.

### 3. Disabling overflow-anchor globally

`overflow-anchor: none` was previously set on the scroll container,
which disabled all native scroll anchoring. It should only be on
elements whose growth *causes* scroll disruption (output areas), not
on the container that needs anchoring.

### 4. Forgetting to cancel scroll pins on user interaction

The ResizeObserver pin must cancel on wheel, touch, and keyboard
scroll events. Without this, the pin fights the user's intentional
scrolling — the viewport keeps snapping back to the focused cell.

### 5. Adding layout pulses without requestAnimationFrame

Firing resize events synchronously can cause layout thrashing. Always
schedule through `requestAnimationFrame`:

```typescript
// WRONG
window.dispatchEvent(new Event("resize"));

// CORRECT
requestAnimationFrame(() => {
  window.dispatchEvent(new Event("resize"));
});
```
