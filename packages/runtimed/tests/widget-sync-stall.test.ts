/**
 * Widget-sync stall reproduction against real WASM.
 *
 * These tests script the matplotlib `@interact` stall symptom entirely
 * through the sync engine + real runtimed-wasm, no React or Tauri. The
 * goal is a regression harness: after we refactor widget sync (drop the
 * optimistic path, make the WidgetStore a pure CRDT projection, wire
 * SyncError recovery), these assertions lock in the expected shape of
 * `commChanges$` emissions for the scenarios that were subtly broken.
 *
 * Scenario shape:
 *   - Daemon opens a FloatSlider + Output widget (mirrors @interact).
 *   - Kernel rapidly echoes slider `value` updates.
 *   - Between slider echoes, the Output widget's `outputs` state turns
 *     over (new plot image).
 *   - The engine's `commChanges$` observable is the UI's single source
 *     of truth — every state change that reaches the frontend must
 *     appear there, in order, keyed by the right commId.
 */

import { type Subscription } from "rxjs";
import { afterEach, beforeEach, describe, expect, it } from "vite-plus/test";
import type { CommChanges } from "../src/comm-diff";
import { type WasmHarness, createWasmHarness } from "./wasm-harness";

// ── Fixtures ────────────────────────────────────────────────────────

const SLIDER_COMM = "slider-comm-id-0000000000000000000";
const OUTPUT_COMM = "output-comm-id-0000000000000000000";

const sliderOpts = {
  targetName: "jupyter.widget",
  modelModule: "@jupyter-widgets/controls",
  modelName: "FloatSliderModel",
  state: {
    _model_module: "@jupyter-widgets/controls",
    _model_module_version: "2.0.0",
    _model_name: "FloatSliderModel",
    _view_module: "@jupyter-widgets/controls",
    _view_module_version: "2.0.0",
    _view_name: "FloatSliderView",
    continuous_update: true,
    description: "Frequency",
    max: 5,
    min: 0.5,
    step: 0.1,
    value: 1,
  },
};

const outputOpts = {
  targetName: "jupyter.widget",
  modelModule: "@jupyter-widgets/output",
  modelName: "OutputModel",
  state: {
    _model_module: "@jupyter-widgets/output",
    _model_module_version: "1.0.0",
    _model_name: "OutputModel",
    _view_module: "@jupyter-widgets/output",
    _view_module_version: "1.0.0",
    _view_name: "OutputView",
    msg_id: "",
    outputs: [] as unknown[],
  },
  seq: 1,
};

// ── Tests ───────────────────────────────────────────────────────────

describe("widget sync: real WASM comm-state pipeline", { retry: 2 }, () => {
  let h: WasmHarness;
  let emissions: CommChanges[];
  let sub: Subscription;

  beforeEach(async () => {
    h = await createWasmHarness("widget-sync-test");
    emissions = [];
    sub = h.engine.commChanges$.subscribe((c) => {
      emissions.push(c);
    });
    await h.startAndCompleteSync();
  });

  afterEach(() => {
    sub.unsubscribe();
    h.dispose();
  });

  it("opens a comm and emits it via commChanges$", async () => {
    h.serverOpenComm(SLIDER_COMM, sliderOpts);
    await h.syncRuntimeState();

    expect(emissions.length).toBeGreaterThan(0);
    const firstOpen = emissions.find((e) => e.opened.length > 0);
    expect(firstOpen).toBeDefined();
    expect(firstOpen?.opened[0]?.commId).toBe(SLIDER_COMM);
    expect(firstOpen?.opened[0]?.state._model_name).toBe("FloatSliderModel");
  });

  it("emits updates separately from opens", async () => {
    h.serverOpenComm(SLIDER_COMM, sliderOpts);
    await h.syncRuntimeState();

    // Single-key update (what the kernel echoes on a slider drag).
    h.serverSetCommState(SLIDER_COMM, { value: 2.5 });
    await h.syncRuntimeState();

    const updated = emissions.find((e) => e.updated.length > 0);
    expect(updated).toBeDefined();
    expect(updated?.updated[0]?.commId).toBe(SLIDER_COMM);
    expect(updated?.updated[0]?.state.value).toBe(2.5);
  });

  it("rapid slider echoes don't drop output widget updates", async () => {
    // Scenario: user hammers the slider. Each server write is a kernel
    // comm_msg echo. Between slider echoes, the Output widget also turns
    // over (matplotlib plt.show produced a new image). The engine must
    // deliver every update for every comm — no merging, no dropping.
    h.serverOpenComm(SLIDER_COMM, sliderOpts);
    h.serverOpenComm(OUTPUT_COMM, outputOpts);
    await h.syncRuntimeState();

    // Five slider updates interleaved with three output turnovers — what
    // @interact looks like when matplotlib plt.show fires every other
    // traitlet change under CPU pressure.
    const ticks: Array<{ slider: number; output?: string }> = [
      { slider: 1.1, output: "sin(1.1x)" },
      { slider: 1.2 },
      { slider: 1.3, output: "sin(1.3x)" },
      { slider: 1.4 },
      { slider: 1.5, output: "sin(1.5x)" },
    ];

    for (const { slider, output } of ticks) {
      h.serverSetCommState(SLIDER_COMM, { value: slider });
      if (output) {
        h.serverSetCommState(OUTPUT_COMM, {
          outputs: [{ output_type: "display_data", data: { "text/plain": output } }],
        });
      }
      await h.syncRuntimeState();
    }

    // Collect the last observed value for each comm.
    const lastState = new Map<string, Record<string, unknown>>();
    for (const batch of emissions) {
      for (const comm of [...batch.opened, ...batch.updated]) {
        lastState.set(comm.commId, { ...lastState.get(comm.commId), ...comm.state });
      }
    }

    expect(lastState.get(SLIDER_COMM)?.value).toBe(1.5);
    // The Output widget's outputs must reflect the FINAL value the
    // server wrote. This is the assertion a future optimistic-path
    // regression would break: the slider's optimistic write would mask
    // the Output widget's CRDT-delivered `outputs` turnover.
    const finalOutputs = lastState.get(OUTPUT_COMM)?.outputs as unknown[];
    expect(finalOutputs).toHaveLength(1);
    expect((finalOutputs[0] as { data: { "text/plain": string } }).data["text/plain"]).toBe(
      "sin(1.5x)",
    );
  });

  it("consecutive distinct updates to the same comm each produce an emission", async () => {
    // Regression guard: the `diffComms` layer compares JSON strings. If
    // two successive server writes produce identical state (e.g. kernel
    // idempotent echo), the second one is coalesced away — but two
    // *distinct* writes must each fire.
    h.serverOpenComm(SLIDER_COMM, sliderOpts);
    await h.syncRuntimeState();

    const beforeCount = emissions.filter((e) => e.updated.length > 0).length;

    h.serverSetCommState(SLIDER_COMM, { value: 3.14 });
    await h.syncRuntimeState();

    h.serverSetCommState(SLIDER_COMM, { value: 2.71 });
    await h.syncRuntimeState();

    const afterCount = emissions.filter((e) => e.updated.length > 0).length;
    expect(afterCount - beforeCount).toBe(2);
  });

  it("resolved state projects native scalars 1:1 from the CRDT", async () => {
    // The WASM resolver turns Automerge-native maps/lists into plain JS
    // objects/arrays. Verify for a minimal slider — no ContentRefs in
    // the picture, so this should be a direct 1:1 projection.
    h.serverOpenComm(SLIDER_COMM, sliderOpts);
    await h.syncRuntimeState();

    const firstOpen = emissions.find((e) => e.opened.length > 0);
    const comm = firstOpen?.opened[0];
    expect(comm?.state.min).toBe(0.5);
    expect(comm?.state.max).toBe(5);
    expect(comm?.state.value).toBe(1);
    expect(comm?.state.continuous_update).toBe(true);
  });
});
