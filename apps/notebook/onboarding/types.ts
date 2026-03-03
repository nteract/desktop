/**
 * Status of the daemon during startup or operation.
 * Matches the DaemonProgress enum from Rust.
 */
export type DaemonStatus =
  | { status: "checking" }
  | { status: "installing" }
  | { status: "upgrading" }
  | { status: "starting" }
  | { status: "waiting_for_ready"; attempt: number; max_attempts: number }
  | { status: "ready"; endpoint: string }
  | { status: "failed"; error: string; guidance?: string }
  | null;
