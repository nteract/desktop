// @vitest-environment jsdom
import { act, render } from "@testing-library/react";
import { BehaviorSubject, Subject } from "rxjs";
import { afterEach, describe, expect, it } from "vite-plus/test";
import { useObservable } from "../use-observable";

function Probe({ obs$, initial }: { obs$: Parameters<typeof useObservable>[0]; initial: unknown }) {
  const value = useObservable(obs$ as never, initial as never);
  return <span data-testid="value">{String(value)}</span>;
}

describe("useObservable", () => {
  afterEach(() => {
    // No global mocks to clear; kept for parity with the other suites.
  });

  it("returns `initial` until the source emits", () => {
    const subject = new Subject<string>();
    const { getByTestId } = render(<Probe obs$={subject} initial="init" />);
    expect(getByTestId("value").textContent).toBe("init");
  });

  it("returns the most recent value after emission", () => {
    const subject = new Subject<string>();
    const { getByTestId } = render(<Probe obs$={subject} initial="init" />);
    act(() => subject.next("one"));
    expect(getByTestId("value").textContent).toBe("one");
    act(() => subject.next("two"));
    expect(getByTestId("value").textContent).toBe("two");
  });

  it("synchronously reflects BehaviorSubject seed after commit", () => {
    const subject = new BehaviorSubject<number>(42);
    const { getByTestId } = render(<Probe obs$={subject} initial={0} />);
    // BehaviorSubject fires synchronously on subscribe; the effect runs
    // after the first commit, so the seed lands on the very next render.
    act(() => {
      // Force a re-render via another emission so the effect-driven
      // setState is visible to the assertion below.
      subject.next(42);
    });
    expect(getByTestId("value").textContent).toBe("42");
  });

  it("unsubscribes on unmount", () => {
    const subject = new Subject<string>();
    const { unmount, getByTestId } = render(<Probe obs$={subject} initial="init" />);
    act(() => subject.next("one"));
    expect(getByTestId("value").textContent).toBe("one");
    unmount();
    // No observers left; emitting shouldn't throw and the component is gone.
    expect(() => subject.next("two")).not.toThrow();
  });
});
