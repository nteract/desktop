export interface NotebookStatus {
  window_label: string;
  notebook_id: string;
  display_name: string;
  kernel_status: "idle" | "busy" | "starting" | "error" | null;
  is_dirty: boolean;
}

export type UpgradeStep =
  | { step: "saving_notebooks" }
  | { step: "stopping_kernels" }
  | { step: "closing_windows" }
  | { step: "upgrading_daemon" }
  | { step: "ready" }
  | { step: "failed"; error: string };
