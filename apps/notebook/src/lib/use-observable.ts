import { useEffect, useState } from "react";
import type { Observable } from "rxjs";

/**
 * Subscribe to an RxJS `Observable<T>` from a React component.
 *
 * Returns the most recently emitted value, or `initial` until the
 * observable emits. Re-renders on each emission.
 *
 * First render shows `initial` because the subscription is set up inside
 * `useEffect`, which runs after commit. For `BehaviorSubject` /
 * `ReplaySubject(1)` sources the cached value lands on the next tick.
 * Consumers that need safe defaults (e.g. a disabled button) should pick
 * `initial` to reflect that "not-yet-loaded" state honestly.
 *
 * The observable must be stable across renders — pass a memoized
 * reference (e.g. `engine.sessionStatus$`) rather than a pipeline built
 * inline, or the subscription tears down and rebuilds every render.
 * Memoize pipelines with `useMemo`.
 */
export function useObservable<T>(observable: Observable<T>, initial: T): T {
  const [value, setValue] = useState<T>(initial);
  useEffect(() => {
    const sub = observable.subscribe((next) => setValue(next));
    return () => sub.unsubscribe();
  }, [observable]);
  return value;
}
