import { Monitor, Moon, Sun } from "lucide-react";
import { useEffect, useState } from "react";
import { TelemetryDisclosureCard } from "@/components/TelemetryDisclosureCard";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { PrivacySection } from "../settings/sections/Privacy";

/**
 * Standalone component gallery.
 *
 * A lightweight showcase for the bespoke UI we're iterating on (onboarding
 * states, Settings sections, banners). Runs under Vite dev server without
 * the Tauri shell, daemon, or any IPC — every data-dependent prop is
 * supplied from local state, and any side effect (opening an URL,
 * persisting a setting) is stubbed at the component's edge.
 *
 * Visit: http://localhost:5174/gallery/ when Vite is running.
 */
export default function App() {
  return (
    <div className="min-h-screen bg-background text-foreground">
      <ThemeBar />
      <main className="max-w-4xl mx-auto p-8 space-y-12">
        <header className="space-y-1">
          <h1 className="text-2xl font-semibold tracking-tight">Component gallery</h1>
          <p className="text-sm text-muted-foreground">
            Preview the components powering onboarding and Settings without launching the desktop
            app.
          </p>
        </header>

        <Section
          title="TelemetryDisclosureCard"
          description="Shared disclosure card. Rendered above the onboarding CTA and embedded (with a trailing switch) in Settings → Privacy."
        >
          <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
            <Variant label="default">
              <TelemetryDisclosureCard />
            </Variant>
            <Variant label="with footer slot">
              <TelemetryDisclosureCard
                footer={
                  <div className="flex items-center justify-between pt-1 text-xs text-muted-foreground">
                    <span>Send anonymous daily ping</span>
                    <span className="text-primary">(toggle slot)</span>
                  </div>
                }
              />
            </Variant>
          </div>
        </Section>

        <Section
          title="Onboarding page 2 — CTA block"
          description="What replaces the pre-checked toggle + Get Started button."
        >
          <OnboardingCTAPreview />
        </Section>

        <Section
          title="Settings → Privacy"
          description="Live section, fully interactive. Switch, install ID rotation, and last-ping relative times all work against local state."
        >
          <div className="max-w-lg border rounded-lg p-4 bg-card">
            <PrivacySectionDemo />
          </div>
        </Section>
      </main>
    </div>
  );
}

function Section({
  title,
  description,
  children,
}: {
  title: string;
  description: string;
  children: React.ReactNode;
}) {
  return (
    <section className="space-y-3">
      <div className="space-y-1">
        <h2 className="text-sm font-semibold tracking-wider uppercase text-muted-foreground">
          {title}
        </h2>
        <p className="text-sm text-foreground/80">{description}</p>
      </div>
      <div>{children}</div>
    </section>
  );
}

function Variant({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="space-y-2">
      <span className="text-[10px] uppercase tracking-wider text-muted-foreground/70">{label}</span>
      {children}
    </div>
  );
}

type OnboardingState = "idle" | "python-env-missing" | "ready" | "submitting" | "setup-complete";

function OnboardingCTAPreview() {
  const [state, setState] = useState<OnboardingState>("ready");
  const canProceed = state === "ready" || state === "submitting";
  const isSubmitting = state === "submitting";
  const setupComplete = state === "setup-complete";
  const pythonEnvSelected = state !== "python-env-missing";

  return (
    <div className="space-y-4">
      <StateSelector
        label="Gate state"
        options={[
          ["idle", "idle (not interactive)"],
          ["python-env-missing", "no python env selected"],
          ["ready", "ready to submit"],
          ["submitting", "submitting..."],
          ["setup-complete", "all set!"],
        ]}
        value={state}
        onChange={setState}
      />
      <div className="max-w-sm mx-auto">
        <div className="space-y-3 p-6 border rounded-lg bg-card">
          <TelemetryDisclosureCard />
          <Button disabled={!canProceed || isSubmitting} className="w-full" size="lg">
            {setupComplete
              ? "All set!"
              : canProceed && !isSubmitting
                ? "You can count on me!"
                : !pythonEnvSelected
                  ? "Select a package manager"
                  : "Setting up..."}
          </Button>
          <button
            type="button"
            disabled={!canProceed || isSubmitting}
            className="w-full text-xs text-muted-foreground underline hover:text-foreground disabled:opacity-50 py-1"
          >
            Opt out of metrics, continue
          </button>
        </div>
      </div>
    </div>
  );
}

function PrivacySectionDemo() {
  const [telemetryEnabled, setTelemetryEnabled] = useState(true);
  const [installId, setInstallId] = useState("c1d4e7f2-8a3b-4f1c-9e2a-1234567890ab");
  const now = Math.floor(Date.now() / 1000);
  const [lastDaemonPingAt] = useState<number | null>(now - 14 * 3600);
  const [lastAppPingAt] = useState<number | null>(now - 2 * 3600);
  const [lastMcpPingAt] = useState<number | null>(null);

  const rotateInstallId = async () => {
    // Fake UUID so the demo feels live.
    const next = crypto.randomUUID();
    setInstallId(next);
    return next;
  };

  return (
    <PrivacySection
      telemetryEnabled={telemetryEnabled}
      onTelemetryChange={setTelemetryEnabled}
      installId={installId}
      onRotate={rotateInstallId}
      lastDaemonPingAt={lastDaemonPingAt}
      lastAppPingAt={lastAppPingAt}
      lastMcpPingAt={lastMcpPingAt}
    />
  );
}

function StateSelector<T extends string>({
  label,
  options,
  value,
  onChange,
}: {
  label: string;
  options: Array<[T, string]>;
  value: T;
  onChange: (value: T) => void;
}) {
  return (
    <div className="flex items-center gap-3 text-sm">
      <span className="text-muted-foreground">{label}</span>
      <div className="flex flex-wrap items-center gap-1 rounded-md border bg-muted/50 p-0.5">
        {options.map(([v, human]) => (
          <button
            key={v}
            type="button"
            onClick={() => onChange(v)}
            className={cn(
              "rounded-sm px-2 py-1 text-xs transition-colors",
              value === v
                ? "bg-background text-foreground shadow-sm"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            {human}
          </button>
        ))}
      </div>
    </div>
  );
}

type ThemeMode = "light" | "dark" | "system";

const THEME_STORAGE_KEY = "notebook-theme";

function ThemeBar() {
  const [theme, setTheme] = useState<ThemeMode>(() => {
    try {
      const stored = localStorage.getItem(THEME_STORAGE_KEY);
      if (stored === "light" || stored === "dark" || stored === "system") {
        return stored;
      }
    } catch {
      /* ignore */
    }
    return "system";
  });

  useEffect(() => {
    try {
      localStorage.setItem(THEME_STORAGE_KEY, theme);
    } catch {
      /* ignore */
    }
    const resolved =
      theme === "system"
        ? window.matchMedia("(prefers-color-scheme: dark)").matches
          ? "dark"
          : "light"
        : theme;
    const html = document.documentElement;
    html.classList.remove("light", "dark");
    html.classList.add(resolved);
  }, [theme]);

  return (
    <div className="flex items-center justify-end gap-1 border-b bg-muted/40 px-4 py-2">
      {(
        [
          { value: "light", icon: Sun, label: "Light" },
          { value: "dark", icon: Moon, label: "Dark" },
          { value: "system", icon: Monitor, label: "System" },
        ] as const
      ).map(({ value, icon: Icon, label }) => (
        <button
          key={value}
          type="button"
          onClick={() => setTheme(value)}
          className={cn(
            "flex items-center gap-1.5 rounded-md px-2 py-1 text-xs transition-colors",
            theme === value
              ? "bg-background text-foreground shadow-sm"
              : "text-muted-foreground hover:text-foreground",
          )}
        >
          <Icon className="h-3.5 w-3.5" />
          {label}
        </button>
      ))}
    </div>
  );
}
