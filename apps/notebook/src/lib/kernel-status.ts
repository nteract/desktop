export const KERNEL_STATUS = {
  NOT_STARTED: "not_started",
  STARTING: "starting",
  IDLE: "idle",
  BUSY: "busy",
  ERROR: "error",
  SHUTDOWN: "shutdown",
} as const;

export type KernelStatus = (typeof KERNEL_STATUS)[keyof typeof KERNEL_STATUS];

const KERNEL_STATUS_SET: ReadonlySet<KernelStatus> = new Set(
  Object.values(KERNEL_STATUS),
);

export const KERNEL_STATUS_LABELS: Record<KernelStatus, string> = {
  [KERNEL_STATUS.NOT_STARTED]: "initializing",
  [KERNEL_STATUS.STARTING]: "starting",
  [KERNEL_STATUS.IDLE]: "idle",
  [KERNEL_STATUS.BUSY]: "busy",
  [KERNEL_STATUS.ERROR]: "error",
  [KERNEL_STATUS.SHUTDOWN]: "shutdown",
};

export function isKernelStatus(value: string): value is KernelStatus {
  return KERNEL_STATUS_SET.has(value as KernelStatus);
}

const STARTING_PHASE_LABELS: Record<string, string> = {
  resolving: "resolving environment",
  preparing_env: "preparing environment",
  launching: "launching kernel",
  connecting: "connecting to kernel",
};

export function getKernelStatusLabel(
  status: KernelStatus,
  startingPhase?: string,
): string {
  if (status === KERNEL_STATUS.STARTING && startingPhase) {
    return STARTING_PHASE_LABELS[startingPhase] ?? "starting";
  }
  return KERNEL_STATUS_LABELS[status];
}
