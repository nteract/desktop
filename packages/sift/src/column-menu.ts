/**
 * Bridge between the imperative table engine and the React context menu.
 * Manages a React root for the ColumnContextMenu component.
 */
import { createRoot, type Root } from 'react-dom/client'
import { createElement } from 'react'
import {
  ColumnContextMenu,
  type ColumnMenuState,
  type ColumnAction,
} from './components/column-context-menu'

let root: Root | null = null
let container: HTMLDivElement | null = null

export type { ColumnAction, ColumnMenuState }

export function mountColumnMenu(
  state: ColumnMenuState,
  onAction: (colIndex: number, action: ColumnAction) => void,
) {
  if (!container) {
    container = document.createElement('div')
    container.id = 'column-context-menu-root'
    document.body.appendChild(container)
    root = createRoot(container)
  }

  function onClose() {
    root?.render(createElement(ColumnContextMenu, { state: null, onAction, onClose }))
  }

  root!.render(createElement(ColumnContextMenu, { state, onAction, onClose }))
}

export function unmountColumnMenu() {
  if (root) {
    root.unmount()
    root = null
  }
  if (container) {
    container.remove()
    container = null
  }
}
