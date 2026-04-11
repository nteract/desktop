import type React from "react";
import { Badge } from "@/components/ui/badge";

interface ExecutionStatusProps {
  executionState: string;
}

export const ExecutionStatus: React.FC<ExecutionStatusProps> = ({ executionState }) => {
  switch (executionState) {
    case "idle":
      return null;
    case "queued":
      return (
        <Badge data-slot="execution-status" variant="secondary" className="h-5 text-xs">
          Queued
        </Badge>
      );
    case "running":
      return (
        <Badge
          data-slot="execution-status"
          variant="outline"
          className="h-5 border-blue-200 bg-blue-50 text-xs text-blue-700"
        >
          <div className="mr-1 h-2 w-2 animate-spin rounded-full border border-blue-600 border-t-transparent"></div>
          Running
        </Badge>
      );
    case "error":
      return (
        <Badge
          data-slot="execution-status"
          variant="outline"
          className="h-5 border-red-200 bg-red-50 text-xs text-red-700"
        >
          Error
        </Badge>
      );
    default:
      return null;
  }
};
