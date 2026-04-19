# Output + widget render audit

Scope: find object-creation-in-props hot paths at the output and widget
boundary that might cause excess re-renders or alloc traffic during
streaming execution. Focused on `App.tsx`, `OutputArea.tsx`,
`CodeCell.tsx`, and the two providers (`MediaProvider`,
`WidgetStoreProvider`).

## Findings

### 1. `resetKeys={[JSON.stringify(output)]}` — real hot path

`src/components/cell/OutputArea.tsx:537` wraps each in-DOM output in an
`ErrorBoundary` with `resetKeys={[JSON.stringify(output)]}`.

For the non-isolated render path (plain text, safe HTML, markdown), the
entire output is serialized to JSON on every parent re-render. A cell
streaming a large build log (hundreds of KB of `text/plain`) pays an
O(size) string allocation + full-object walk per render.

Outputs are replaced by reference when the cell store writes a new array
(`replaceNotebookCells` → per-cell updates), so shallow reference
comparison is sufficient.

Fix: `resetKeys={[output]}` — React's `ErrorBoundary` resetKeys use
`Object.is` comparison.

### 2. `style={{ maxHeight: ... }}` — fresh object per render

`src/components/cell/OutputArea.tsx:510`:

```tsx
style={maxHeight ? { maxHeight: `${maxHeight}px` } : undefined}
```

Fresh object every render when `maxHeight` is set. Minor — React's
reconciler handles style objects by diff, and `maxHeight` rarely changes.
Fix with `useMemo` if it shows up in a profile.

### 3. `<MediaProvider renderers={{...}}>` in App root — brittle, not currently hot

`apps/notebook/src/App.tsx:1384` passes a fresh `renderers` object and a
fresh inline arrow on every render of `App()`. Today `App()` has no
state and never re-renders, so the provider value is stable. This is a
footgun: adding any hook or state to `App` would immediately regress
the whole output tree (every MediaRouter consumer re-renders on every
App render).

Low-cost hardening: hoist the renderers object to a module-level
constant and wrap the inline arrow in a named `WidgetViewRenderer`
component. Pure cleanup, no behavior change.

## Non-findings

- `WidgetStoreProvider` memoizes its context value with `useMemo` over
  `store, handleMessage, sendMessage, sendUpdate, sendCustom, closeComm`
  (`src/components/widgets/widget-store-context.tsx:95`). Store is
  stable via `useRef`; router callbacks come from `useCommRouter`.
  Assuming `useCommRouter` returns stable identities (spot-checked),
  consumers don't re-render on widget state churn.
- `MediaProvider` recomputes its value object every render but only
  runs when its parent renders — App-root today, so effectively once.
- `CodeCell` already wraps its hot callbacks in `useCallback` /
  `useMemo` (`handleFocusNextOrCreate`, `keyMap`, `editorExtensions`,
  `handleLinkClick`).
- `OutputArea.handleFrameReady` depends on `outputs` — intentional;
  the iframe re-renders when outputs change. The `useEffect` at line
  442 is the desired trigger.

## Impact ordering

1. Fix resetKeys serialization — concrete waste on streaming cells,
   trivial diff.
2. Hoist the App-root renderers object — hardens against a future
   App-becomes-stateful regression.
3. Memoize the OutputArea style — only if it shows up.

No evidence of widget-boundary alloc storms from this pass. The
`WidgetStoreProvider` / `MediaProvider` shape is sound; the real
cost is a JSON.stringify that got tacked onto an ErrorBoundary.
