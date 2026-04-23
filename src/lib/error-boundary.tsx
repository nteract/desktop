import { Component, type ErrorInfo, type ReactNode } from "react";

/**
 * Optional sink for caught render errors. Wire this to the host logger from
 * the app entry so CI / production builds capture the React error and its
 * component stack — the console.error in componentDidCatch is only visible
 * in dev-mode devtools.
 */
type ErrorBoundarySink = (
  error: Error,
  componentStack: string | null | undefined,
) => void;

let _sink: ErrorBoundarySink | null = null;

export function setErrorBoundarySink(sink: ErrorBoundarySink | null): void {
  _sink = sink;
}

export interface ErrorBoundaryProps {
  children: ReactNode;
  /** Render prop called when an error is caught. Return the fallback UI. */
  fallback: (error: Error, resetErrorBoundary: () => void) => ReactNode;
  /** Optional callback when an error is caught. */
  onError?: (error: Error, errorInfo: ErrorInfo) => void;
  /**
   * When any value in this array changes, the error state is automatically
   * cleared and the children are re-rendered. Use this to recover from
   * errors when the underlying data changes (e.g., cell re-execution,
   * new output data, widget state update).
   */
  resetKeys?: readonly unknown[];
}

interface ErrorBoundaryState {
  error: Error | null;
  prevResetKeys: readonly unknown[] | undefined;
}

/**
 * Reusable React error boundary.
 *
 * Catches render errors in child components and displays a fallback UI
 * via the `fallback` render prop. Provides a `resetErrorBoundary` function
 * to retry rendering.
 *
 * Supports automatic recovery via `resetKeys` — when any key changes,
 * the error state is cleared and children re-render.
 *
 * @example
 * ```tsx
 * <ErrorBoundary
 *   fallback={(error, reset) => (
 *     <div>
 *       <p>Something went wrong: {error.message}</p>
 *       <button onClick={reset}>Retry</button>
 *     </div>
 *   )}
 *   resetKeys={[executionCount]}
 * >
 *   <MyComponent />
 * </ErrorBoundary>
 * ```
 */
export class ErrorBoundary extends Component<
  ErrorBoundaryProps,
  ErrorBoundaryState
> {
  constructor(props: ErrorBoundaryProps) {
    super(props);
    this.state = { error: null, prevResetKeys: props.resetKeys };
  }

  static getDerivedStateFromError(error: Error): Partial<ErrorBoundaryState> {
    return { error };
  }

  static getDerivedStateFromProps(
    props: ErrorBoundaryProps,
    state: ErrorBoundaryState,
  ): Partial<ErrorBoundaryState> | null {
    if (
      state.error &&
      state.prevResetKeys !== undefined &&
      props.resetKeys !== undefined
    ) {
      const changed =
        props.resetKeys.length !== state.prevResetKeys.length ||
        props.resetKeys.some(
          (key, i) => !Object.is(key, state.prevResetKeys![i]),
        );
      if (changed) {
        return { error: null, prevResetKeys: props.resetKeys };
      }
    }
    // Always track latest resetKeys even when not in error state
    if (props.resetKeys !== state.prevResetKeys) {
      return { prevResetKeys: props.resetKeys };
    }
    return null;
  }

  componentDidCatch(error: Error, errorInfo: ErrorInfo) {
    console.error("[ErrorBoundary]", error, errorInfo.componentStack);
    try {
      _sink?.(error, errorInfo.componentStack);
    } catch {
      // Sink must never throw from the error path.
    }
    this.props.onError?.(error, errorInfo);
  }

  resetErrorBoundary = () => {
    this.setState({ error: null });
  };

  render() {
    if (this.state.error) {
      return this.props.fallback(this.state.error, this.resetErrorBoundary);
    }
    return this.props.children;
  }
}
