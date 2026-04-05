import { AlertCircle, ChevronDown, Monitor, Moon, Sun, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { Slider } from "@/components/ui/slider";
import { Switch } from "@/components/ui/switch";
import {
  isKnownPythonEnv,
  isKnownRuntime,
  type ThemeMode,
  useSyncedSettings,
  useSyncedTheme,
} from "@/hooks/useSyncedSettings";
import { cn } from "@/lib/utils";
import {
  CondaIcon,
  DenoIcon,
  PixiIcon,
  PythonIcon,
  UvIcon,
} from "../src/components/icons";

/** Format seconds into human-readable duration */
function formatDuration(secs: number): string {
  if (secs >= 86400) {
    const days = Math.floor(secs / 86400);
    const hours = Math.floor((secs % 86400) / 3600);
    return hours > 0 ? `${days}d ${hours}h` : `${days}d`;
  }
  if (secs >= 3600) {
    const hours = Math.floor(secs / 3600);
    const mins = Math.floor((secs % 3600) / 60);
    return mins > 0 ? `${hours}h ${mins}m` : `${hours}h`;
  }
  if (secs >= 60) {
    const mins = Math.floor(secs / 60);
    const remainingSecs = secs % 60;
    return remainingSecs > 0 ? `${mins}m ${remainingSecs}s` : `${mins}m`;
  }
  return `${secs}s`;
}

// Exponential slider constants
const MIN_SECS = 5;
const MAX_SECS = 604800; // 7 days
const SLIDER_STEPS = 100;

// Convert slider position (0-100) to seconds (exponential scale)
function sliderToSeconds(position: number): number {
  const ratio = MAX_SECS / MIN_SECS;
  const secs = Math.round(MIN_SECS * ratio ** (position / SLIDER_STEPS));
  return Math.max(MIN_SECS, Math.min(MAX_SECS, secs));
}

// Convert seconds to slider position (0-100)
function secondsToSlider(secs: number): number {
  const ratio = MAX_SECS / MIN_SECS;
  const position = (SLIDER_STEPS * Math.log(secs / MIN_SECS)) / Math.log(ratio);
  return Math.max(0, Math.min(SLIDER_STEPS, Math.round(position)));
}

/** Keep Alive slider - exponential scale from 5s to 7 days */
function KeepAliveSlider({
  value,
  onChange,
}: {
  value: number;
  onChange: (value: number) => void;
}) {
  const [localValue, setLocalValue] = useState(value);

  useEffect(() => {
    setLocalValue(value);
  }, [value]);

  const sliderPosition = secondsToSlider(localValue);

  return (
    <div className="space-y-3 pt-4 border-t border-border/50">
      <div>
        <span className="text-xs font-semibold text-muted-foreground uppercase tracking-wider">
          Advanced
        </span>
      </div>
      <div className="space-y-1">
        <div className="flex items-center justify-between">
          <span className="text-xs font-medium text-muted-foreground">
            Keep Alive
          </span>
          <span className="text-xs font-medium text-foreground tabular-nums">
            {formatDuration(localValue)}
          </span>
        </div>
        <p className="text-[10px] text-muted-foreground/70">
          Time to keep notebook runtime alive after closing
        </p>
      </div>
      <div className="py-2">
        <Slider
          value={[sliderPosition]}
          min={0}
          max={SLIDER_STEPS}
          step={1}
          onValueChange={(v) => setLocalValue(sliderToSeconds(v[0]))}
          onValueCommit={(v) => onChange(sliderToSeconds(v[0]))}
        />
      </div>
      <div className="flex justify-between text-[10px] text-muted-foreground/70">
        <span>5s</span>
        <span>7 days</span>
      </div>
    </div>
  );
}

/** Badge input for managing a list of package names */
function PackageBadgeInput({
  packages,
  onChange,
  placeholder,
}: {
  packages: string[];
  onChange: (packages: string[]) => void;
  placeholder?: string;
}) {
  const [inputValue, setInputValue] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  const addPackage = useCallback(
    (raw: string) => {
      const name = raw.trim();
      if (!name) return;
      if (!packages.includes(name)) {
        onChange([...packages, name]);
      }
      setInputValue("");
    },
    [packages, onChange],
  );

  const removePackage = useCallback(
    (index: number) => {
      onChange(packages.filter((_, i) => i !== index));
    },
    [packages, onChange],
  );

  return (
    <div
      className="flex flex-wrap items-center gap-1 min-h-7 rounded-md border bg-muted/50 px-1.5 py-1 cursor-text"
      onClick={() => inputRef.current?.focus()}
    >
      {packages.map((pkg, i) => (
        <span
          key={`${pkg}-${i}`}
          className="inline-flex items-center gap-0.5 rounded-md bg-secondary text-secondary-foreground pl-1.5 pr-0.5 py-0 text-xs leading-5"
        >
          {pkg}
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              removePackage(i);
            }}
            className="rounded-sm p-0 hover:bg-muted-foreground/20"
          >
            <X className="h-3 w-3" />
          </button>
        </span>
      ))}
      <input
        ref={inputRef}
        type="text"
        value={inputValue}
        onChange={(e) => setInputValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            addPackage(inputValue);
          } else if (
            e.key === "Backspace" &&
            inputValue === "" &&
            packages.length > 0
          ) {
            removePackage(packages.length - 1);
          }
        }}
        onBlur={() => {
          if (inputValue.trim()) {
            addPackage(inputValue);
          }
        }}
        placeholder={packages.length === 0 ? placeholder : ""}
        autoComplete="off"
        autoCorrect="off"
        autoCapitalize="off"
        spellCheck={false}
        className="flex-1 min-w-[80px] bg-transparent text-xs text-foreground placeholder:text-muted-foreground focus:outline-none h-5"
      />
    </div>
  );
}

interface FeatureFlag {
  id: string;
  label: string;
  description: string;
  settingKey: string;
}

const FEATURE_FLAGS: FeatureFlag[] = [];

const themeOptions: { value: ThemeMode; label: string; icon: typeof Sun }[] = [
  { value: "light", label: "Light", icon: Sun },
  { value: "dark", label: "Dark", icon: Moon },
  { value: "system", label: "System", icon: Monitor },
];

export default function App() {
  // Apply theme to window and get theme controls
  // IMPORTANT: Use theme/setTheme from useSyncedTheme, not a separate useSyncedSettings call,
  // so that setTheme updates the same state instance that applies the DOM theme.
  const { theme, setTheme } = useSyncedTheme();

  const {
    defaultRuntime,
    setDefaultRuntime,
    defaultPythonEnv,
    setDefaultPythonEnv,
    defaultUvPackages,
    setDefaultUvPackages,
    defaultCondaPackages,
    setDefaultCondaPackages,
    defaultPixiPackages,
    setDefaultPixiPackages,
    keepAliveSecs,
    setKeepAliveSecs,
  } = useSyncedSettings();

  return (
    <div className="h-full overflow-auto">
      <div className="p-6 space-y-6 max-w-lg mx-auto">
        <h1 className="text-lg font-semibold">Settings</h1>

        {/* Theme */}
        <div className="space-y-2">
          <span className="text-xs font-semibold text-muted-foreground uppercase tracking-wider">
            Appearance
          </span>
          <div className="flex items-center gap-3">
            <span className="text-sm text-muted-foreground">Theme</span>
            <div className="flex items-center gap-1 rounded-md border bg-muted/50 p-0.5">
              {themeOptions.map((option) => {
                const Icon = option.icon;
                const isActive = theme === option.value;
                return (
                  <button
                    key={option.value}
                    type="button"
                    onClick={() => setTheme(option.value)}
                    className={cn(
                      "flex items-center gap-1.5 rounded-sm px-2.5 py-1 text-xs transition-colors",
                      isActive
                        ? "bg-background text-foreground shadow-sm"
                        : "text-muted-foreground hover:text-foreground",
                    )}
                  >
                    <Icon className="h-3.5 w-3.5" />
                    {option.label}
                  </button>
                );
              })}
            </div>
          </div>
        </div>

        {/* Default Runtime */}
        <div className="space-y-2">
          <span className="text-xs font-semibold text-muted-foreground uppercase tracking-wider">
            New Notebooks
          </span>
          <div className="space-y-1">
            <div className="flex items-center gap-3">
              <span className="text-sm text-muted-foreground">
                Default Runtime
              </span>
              <div className="flex items-center gap-1 rounded-md border bg-muted/50 p-0.5">
                <button
                  type="button"
                  onClick={() => setDefaultRuntime("python")}
                  className={cn(
                    "flex items-center gap-1.5 rounded-sm px-2.5 py-1 text-xs transition-colors",
                    defaultRuntime === "python"
                      ? "bg-blue-500/15 text-blue-600 dark:text-blue-400 shadow-sm"
                      : "text-muted-foreground hover:text-foreground",
                  )}
                >
                  <PythonIcon className="h-3.5 w-3.5" />
                  Python
                </button>
                <button
                  type="button"
                  onClick={() => setDefaultRuntime("deno")}
                  className={cn(
                    "flex items-center gap-1.5 rounded-sm px-2.5 py-1 text-xs transition-colors",
                    defaultRuntime === "deno"
                      ? "bg-teal-500/15 text-teal-600 dark:text-teal-400 shadow-sm"
                      : "text-muted-foreground hover:text-foreground",
                  )}
                >
                  <DenoIcon className="h-3.5 w-3.5" />
                  Deno
                </button>
              </div>
            </div>
            {defaultRuntime && !isKnownRuntime(defaultRuntime) && (
              <div className="flex items-start gap-2 text-xs text-amber-700 dark:text-amber-400 mt-1">
                <AlertCircle className="h-3.5 w-3.5 mt-0.5 shrink-0" />
                <span>
                  <span className="font-medium">
                    &ldquo;{defaultRuntime}&rdquo;
                  </span>{" "}
                  is not a recognized runtime. Click Python or Deno above, or
                  edit{" "}
                  <code className="rounded bg-amber-500/20 px-1">
                    settings.json
                  </code>
                  .
                </span>
              </div>
            )}
          </div>
        </div>

        {/* Python Defaults */}
        <div className="space-y-3">
          <div>
            <span className="text-xs font-semibold text-muted-foreground uppercase tracking-wider">
              Python Defaults
            </span>
            <p className="text-[11px] text-muted-foreground/70 mt-0.5">
              Applied to new notebooks without project-based dependencies
            </p>
          </div>
          <div
            className="grid gap-3"
            style={{ gridTemplateColumns: "auto 1fr" }}
          >
            {/* Default Python Env */}
            <span className="text-sm text-muted-foreground whitespace-nowrap self-center text-right">
              Environment
            </span>
            <div className="flex items-center gap-1 rounded-md border bg-muted/50 p-0.5 w-fit">
              <button
                type="button"
                onClick={() => setDefaultPythonEnv("uv")}
                className={cn(
                  "flex items-center gap-1.5 rounded-sm px-2.5 py-1 text-xs transition-colors",
                  defaultPythonEnv === "uv"
                    ? "bg-fuchsia-500/15 text-fuchsia-600 dark:text-fuchsia-400 shadow-sm"
                    : "text-muted-foreground hover:text-foreground",
                )}
              >
                <UvIcon className="h-3 w-3" />
                uv
              </button>
              <button
                type="button"
                onClick={() => setDefaultPythonEnv("conda")}
                className={cn(
                  "flex items-center gap-1.5 rounded-sm px-2.5 py-1 text-xs transition-colors",
                  defaultPythonEnv === "conda"
                    ? "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400 shadow-sm"
                    : "text-muted-foreground hover:text-foreground",
                )}
              >
                <CondaIcon className="h-3 w-3" />
                Conda
              </button>
              <button
                type="button"
                onClick={() => setDefaultPythonEnv("pixi")}
                className={cn(
                  "flex items-center gap-1.5 rounded-sm px-2.5 py-1 text-xs transition-colors",
                  defaultPythonEnv === "pixi"
                    ? "bg-amber-500/15 text-amber-600 dark:text-amber-400 shadow-sm"
                    : "text-muted-foreground hover:text-foreground",
                )}
              >
                <PixiIcon className="h-3 w-3" />
                Pixi
              </button>
            </div>
            {defaultPythonEnv && !isKnownPythonEnv(defaultPythonEnv) && (
              <div className="flex items-start gap-2 text-xs text-amber-700 dark:text-amber-400 col-span-2 mt-1">
                <AlertCircle className="h-3.5 w-3.5 mt-0.5 shrink-0" />
                <span>
                  <span className="font-medium">
                    &ldquo;{defaultPythonEnv}&rdquo;
                  </span>{" "}
                  is not a recognized environment. Click uv, Conda, or Pixi above, or
                  edit{" "}
                  <code className="rounded bg-amber-500/20 px-1">
                    settings.json
                  </code>
                  .
                </span>
              </div>
            )}

            {/* Packages */}
            {defaultPythonEnv === "uv" && (
              <>
                <span className="text-sm text-muted-foreground whitespace-nowrap self-center text-right">
                  Packages
                </span>
                <PackageBadgeInput
                  packages={defaultUvPackages}
                  onChange={setDefaultUvPackages}
                  placeholder="Add packages..."
                />
              </>
            )}
            {defaultPythonEnv === "conda" && (
              <>
                <span className="text-sm text-muted-foreground whitespace-nowrap self-center text-right">
                  Packages
                </span>
                <PackageBadgeInput
                  packages={defaultCondaPackages}
                  onChange={setDefaultCondaPackages}
                  placeholder="Add packages..."
                />
              </>
            )}
            {defaultPythonEnv === "pixi" && (
              <>
                <span className="text-sm text-muted-foreground whitespace-nowrap self-center text-right">
                  Packages
                </span>
                <PackageBadgeInput
                  packages={defaultPixiPackages}
                  onChange={setDefaultPixiPackages}
                  placeholder="Add packages..."
                />
              </>
            )}
          </div>
        </div>

        {/* Advanced */}
        <KeepAliveSlider value={keepAliveSecs} onChange={setKeepAliveSecs} />

        {/* Feature Flags — only shown when there are flags to display */}
        {FEATURE_FLAGS.length > 0 && (
          <Collapsible className="space-y-2 pt-4 border-t border-border/50">
            <CollapsibleTrigger className="flex items-center gap-1.5 w-full group">
              <ChevronDown className="h-3.5 w-3.5 text-muted-foreground transition-transform group-data-[state=closed]:-rotate-90" />
              <span className="text-xs font-semibold text-muted-foreground uppercase tracking-wider">
                Feature Flags
              </span>
            </CollapsibleTrigger>
            <CollapsibleContent className="space-y-3">
              {FEATURE_FLAGS.map((flag) => (
                <div
                  key={flag.id}
                  className="flex items-center justify-between pl-5"
                >
                  <div className="space-y-0.5">
                    <span className="text-sm text-foreground">
                      {flag.label}
                    </span>
                    <p className="text-[10px] text-muted-foreground/70">
                      {flag.description}
                    </p>
                  </div>
                  <Switch
                    checked={false}
                    onCheckedChange={() => {}}
                  />
                </div>
              ))}
            </CollapsibleContent>
          </Collapsible>
        )}
      </div>
    </div>
  );
}
