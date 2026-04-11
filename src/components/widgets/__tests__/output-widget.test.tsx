import { act, render, screen } from "@testing-library/react";
import { beforeAll, describe, expect, it } from "vite-plus/test";
import { OutputWidget } from "@/components/widgets/controls/output-widget";
import { createWidgetStore } from "@/components/widgets/widget-store";
import { WidgetStoreContext } from "@/components/widgets/widget-store-context";

describe("OutputWidget", () => {
  beforeAll(() => {
    if (!window.matchMedia) {
      Object.defineProperty(window, "matchMedia", {
        writable: true,
        value: (query: string) => ({
          media: query,
          matches: false,
          onchange: null,
          addListener: () => {},
          removeListener: () => {},
          addEventListener: () => {},
          removeEventListener: () => {},
          dispatchEvent: () => false,
        }),
      });
    }
  });

  it("renders outputs from widget store state", async () => {
    const store = createWidgetStore();
    store.createModel("output-1", {
      _model_name: "OutputModel",
      _model_module: "@jupyter-widgets/output",
      outputs: [],
    });

    render(
      <WidgetStoreContext.Provider
        value={{
          store,
          handleMessage: () => {},
          sendMessage: () => {},
          sendUpdate: () => {},
          sendCustom: () => {},
          closeComm: () => {},
        }}
      >
        <OutputWidget modelId="output-1" />
      </WidgetStoreContext.Provider>,
    );

    await act(async () => {});

    // Simulate daemon writing resolved outputs to widget state
    // (via CRDT watcher → WidgetStore updateModel)
    act(() => {
      store.updateModel("output-1", {
        outputs: [
          {
            output_type: "stream",
            name: "stdout",
            text: "Clicked!",
          },
        ],
      });
    });

    expect(await screen.findByText("Clicked!")).toBeInTheDocument();
  });

  it("clears outputs when state is updated with empty array", async () => {
    const store = createWidgetStore();
    store.createModel("output-1", {
      _model_name: "OutputModel",
      _model_module: "@jupyter-widgets/output",
      outputs: [
        {
          output_type: "stream",
          name: "stdout",
          text: "First",
        },
      ],
    });

    render(
      <WidgetStoreContext.Provider
        value={{
          store,
          handleMessage: () => {},
          sendMessage: () => {},
          sendUpdate: () => {},
          sendCustom: () => {},
          closeComm: () => {},
        }}
      >
        <OutputWidget modelId="output-1" />
      </WidgetStoreContext.Provider>,
    );

    expect(await screen.findByText("First")).toBeInTheDocument();

    // Simulate clear_output via state update (CRDT path)
    act(() => {
      store.updateModel("output-1", { outputs: [] });
    });

    expect(screen.queryByText("First")).not.toBeInTheDocument();
  });
});
