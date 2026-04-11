/**
 * FileUpload widget - file upload button.
 *
 * Maps to ipywidgets FileUploadModel.
 */

import { UploadIcon } from "lucide-react";
import { useCallback, useRef } from "react";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";
import { getButtonStyle } from "./button-style-utils";

interface FileData {
  name: string;
  type: string;
  size: number;
  content: string; // base64
  last_modified: number;
}

export function FileUploadWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();
  const inputRef = useRef<HTMLInputElement>(null);

  // Subscribe to individual state keys
  const value = useWidgetModelValue<FileData[]>(modelId, "value") ?? [];
  const accept = useWidgetModelValue<string>(modelId, "accept") ?? "";
  const multiple = useWidgetModelValue<boolean>(modelId, "multiple") ?? false;
  const description = useWidgetModelValue<string>(modelId, "description") ?? "Upload";
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;
  const buttonStyle = useWidgetModelValue<string>(modelId, "button_style") ?? "";
  const icon = useWidgetModelValue<string>(modelId, "icon") ?? "upload";

  const { variant, className: styleClassName } = getButtonStyle(buttonStyle);

  const handleClick = useCallback(() => {
    inputRef.current?.click();
  }, []);

  const handleFileChange = useCallback(
    async (e: React.ChangeEvent<HTMLInputElement>) => {
      const files = e.target.files;
      if (!files || files.length === 0) return;

      const fileDataArray: FileData[] = [];

      for (const file of Array.from(files)) {
        // Read file as base64
        const content = await new Promise<string>((resolve, reject) => {
          const reader = new FileReader();
          reader.onload = () => {
            const result = reader.result as string;
            // Remove data URL prefix to get just base64
            const base64 = result.split(",")[1] || "";
            resolve(base64);
          };
          reader.onerror = reject;
          reader.readAsDataURL(file);
        });

        fileDataArray.push({
          name: file.name,
          type: file.type,
          size: file.size,
          content,
          last_modified: file.lastModified,
        });
      }

      sendUpdate(modelId, { value: fileDataArray });

      // Reset input so same file can be selected again
      if (inputRef.current) {
        inputRef.current.value = "";
      }
    },
    [modelId, sendUpdate],
  );

  const fileCount = value.length;
  const buttonText =
    fileCount > 0 ? `${fileCount} file${fileCount > 1 ? "s" : ""} selected` : description;

  return (
    <div
      className={cn("flex items-center gap-3", className)}
      data-widget-id={modelId}
      data-widget-type="FileUpload"
    >
      <input
        ref={inputRef}
        type="file"
        accept={accept}
        multiple={multiple}
        onChange={handleFileChange}
        disabled={disabled}
        className="hidden"
      />
      <Button
        variant={variant}
        onClick={handleClick}
        disabled={disabled}
        className={cn(styleClassName, "gap-2")}
      >
        {icon === "upload" && <UploadIcon className="size-4" />}
        {buttonText}
      </Button>
      {fileCount > 0 && (
        <Label className="text-sm text-muted-foreground">
          {value.map((f) => f.name).join(", ")}
        </Label>
      )}
    </div>
  );
}

export default FileUploadWidget;
