import { getCurrentWebview } from "@tauri-apps/api/webview";
import { Observable } from "rxjs";

/**
 * Create an Observable from a Tauri webview event listener.
 *
 * Subscribing starts listening; unsubscribing tears down the listener.
 * Each emission is the `event.payload` — the Tauri `Event<T>` wrapper
 * is unwrapped automatically.
 *
 * ```ts
 * fromTauriEvent<number[]>("notebook:frame").subscribe(payload => {
 *   // payload is number[], not Event<number[]>
 * });
 * ```
 *
 * The returned Observable is **cold** — each subscriber gets its own
 * Tauri listener. Use `share()` or `shareReplay()` to multicast.
 */
export function fromTauriEvent<T>(eventName: string): Observable<T> {
  return new Observable<T>((subscriber) => {
    const webview = getCurrentWebview();

    // webview.listen returns Promise<UnlistenFn>. We stash the promise
    // so we can unlisten on teardown even if subscribe is called before
    // the listener is fully registered.
    const unlistenPromise = webview.listen<T>(eventName, (event) => {
      subscriber.next(event.payload);
    });

    // If the listen call itself rejects (e.g. webview destroyed), surface
    // the error through the Observable.
    unlistenPromise.catch((err: unknown) => {
      subscriber.error(err);
    });

    // Teardown: unlisten when the subscriber unsubscribes.
    return () => {
      unlistenPromise.then((fn) => fn()).catch(() => {});
    };
  });
}
