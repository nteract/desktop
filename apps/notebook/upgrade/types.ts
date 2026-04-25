export interface NotebookStatus {
  window_label: string;
  notebook_id: string;
  display_name: string;
  kernel_status: "idle" | "busy" | "starting" | "error" | "not_started" | null;
}

export type UpgradeStep =
  | { step: "saving_notebooks" }
  | { step: "stopping_runtimes" }
  | { step: "closing_windows" }
  | { step: "upgrading_daemon" }
  | { step: "ready" }
  | { step: "failed"; error: string };

export interface CliMigrationInfo {
  dir: string;
  cli_name: string;
  nb_name: string;
}
