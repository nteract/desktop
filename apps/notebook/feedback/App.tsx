import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-shell";
import { useCallback, useEffect, useState } from "react";
import { useSyncedTheme } from "@/hooks/useSyncedSettings";

interface FeedbackSystemInfo {
  app_version: string;
  commit_sha: string;
  release_date: string;
  os: string;
  arch: string;
  os_version: string;
}

const MAX_URL_LENGTH = 8000;
const GITHUB_ISSUES_URL = "https://github.com/nteract/desktop/issues/new";

export default function App() {
  useSyncedTheme();
  const [message, setMessage] = useState("");
  const [systemInfo, setSystemInfo] = useState<FeedbackSystemInfo | null>(null);
  const [sending, setSending] = useState(false);

  useEffect(() => {
    invoke<FeedbackSystemInfo>("get_feedback_system_info")
      .then(setSystemInfo)
      .catch(() => {});
  }, []);

  const handleSubmit = useCallback(async () => {
    if (!message.trim() || sending) return;
    setSending(true);

    const sysBlock = systemInfo
      ? [
          "",
          "",
          "---",
          "**System Info**",
          `- App: ${systemInfo.app_version} (${systemInfo.commit_sha.slice(0, 8)})`,
          `- OS: ${systemInfo.os} ${systemInfo.os_version} (${systemInfo.arch})`,
          `- Released: ${systemInfo.release_date}`,
        ].join("\n")
      : "";

    const title = encodeURIComponent("In-App Feedback");
    const fullBody = message.trim() + sysBlock;

    // Truncate body if URL would exceed limit
    const suffix = "\n\n[truncated]";
    const encodedSuffix = encodeURIComponent(suffix);
    const overhead =
      GITHUB_ISSUES_URL.length +
      "?title=".length +
      title.length +
      "&body=".length +
      "&labels=feedback".length;
    const bodyBudget = MAX_URL_LENGTH - overhead;
    const encodedBody = encodeURIComponent(fullBody);
    const body =
      encodedBody.length > bodyBudget
        ? encodeURIComponent(
            fullBody.slice(0, Math.floor((bodyBudget - encodedSuffix.length) / 3)),
          ) + encodedSuffix
        : encodedBody;

    const url = `${GITHUB_ISSUES_URL}?title=${title}&body=${body}&labels=feedback`;

    try {
      await open(url);
      getCurrentWindow().close();
    } catch {
      setSending(false);
    }
  }, [message, systemInfo, sending]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
        e.preventDefault();
        handleSubmit();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [handleSubmit]);

  const isMac = navigator.platform.startsWith("Mac");
  const shortcutLabel = isMac ? "\u2318\u21B5" : "Ctrl+\u21B5";

  return (
    <div className="h-full overflow-auto">
      <div className="p-6 space-y-4 max-w-lg mx-auto">
        <h1 className="text-lg font-semibold">Feedback</h1>

        <div>
          <textarea
            value={message}
            onChange={(e) => setMessage(e.target.value)}
            placeholder="Tell us about your experience, bugs you've found, or features you'd like to see..."
            aria-label="Feedback message"
            className="w-full h-48 rounded-md border bg-muted/50 px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-sky-400 resize-y"
            autoFocus
          />
        </div>

        <div className="flex items-center justify-between">
          <p className="text-xs text-muted-foreground">
            You can add screenshots after the issue opens in GitHub.
          </p>
          <button
            type="button"
            onClick={handleSubmit}
            disabled={!message.trim() || sending}
            className="rounded-md bg-sky-500 text-white px-4 py-2 text-sm font-medium hover:bg-sky-600 disabled:opacity-50 disabled:cursor-not-allowed shrink-0 ml-4"
          >
            Send feedback {shortcutLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
