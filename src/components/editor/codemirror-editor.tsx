import {
  EditorView,
  type KeyBinding,
  keymap,
  placeholder as placeholderExt,
} from "@codemirror/view";
import { type Extension, useCodeMirror } from "@uiw/react-codemirror";
import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
} from "react";

import { cn } from "@/lib/utils";
import { defaultExtensions } from "./extensions";
import {
  getIPythonExtension,
  getLanguageExtension,
  type SupportedLanguage,
} from "./languages";
import { darkTheme, isDarkMode, lightTheme, type ThemeMode } from "./themes";

export interface CodeMirrorEditorRef {
  /** Focus the editor */
  focus: () => void;
  /** Set cursor position to start or end of document */
  setCursorPosition: (position: "start" | "end") => void;
  /** Get the underlying EditorView instance */
  getEditor: () => EditorView | null;
}

export interface CodeMirrorEditorProps {
  /** Current editor content */
  value: string;
  /** Language for syntax highlighting */
  language?: SupportedLanguage;
  /** Callback when content changes */
  onValueChange?: (value: string) => void;
  /** Auto-focus on mount */
  autoFocus?: boolean;
  /** Callback when editor gains focus */
  onFocus?: () => void;
  /** Callback when editor loses focus */
  onBlur?: () => void;
  /** Placeholder text when empty */
  placeholder?: string;
  /** Additional key bindings */
  keyMap?: KeyBinding[];
  /** Additional CSS classes */
  className?: string;
  /** Maximum height (CSS value) */
  maxHeight?: string;
  /** Enable line wrapping */
  lineWrapping?: boolean;
  /** Additional CodeMirror extensions */
  extensions?: Extension[];
  /** Replace default extensions entirely */
  baseExtensions?: Extension[];
  /** Read-only mode */
  readOnly?: boolean;
  /** Theme mode: "light", "dark", or "auto" (default) */
  theme?: ThemeMode;
}

/**
 * CodeMirror 6 editor component for notebook cells
 *
 * Provides syntax highlighting, key bindings, and a clean API
 * for integration with notebook cell components.
 *
 * @example
 * ```tsx
 * <CodeMirrorEditor
 *   value={source}
 *   language="python"
 *   onValueChange={setSource}
 *   placeholder="Enter code..."
 * />
 * ```
 */
export const CodeMirrorEditor = forwardRef<
  CodeMirrorEditorRef,
  CodeMirrorEditorProps
>(
  (
    {
      value,
      language = "python",
      onValueChange,
      autoFocus = false,
      onFocus,
      onBlur,
      placeholder,
      keyMap,
      className,
      maxHeight,
      lineWrapping = false,
      extensions: additionalExtensions,
      baseExtensions = defaultExtensions,
      readOnly = false,
      theme = "system",
    },
    ref,
  ) => {
    const editorRef = useRef<HTMLDivElement | null>(null);
    const editorViewRef = useRef<EditorView | null>(null);

    // Track dark mode state for "system" theme
    const [isDark, setIsDark] = useState(() =>
      typeof window !== "undefined" ? isDarkMode() : false,
    );

    // Listen for dark mode changes (system preference + document class)
    useEffect(() => {
      if (theme !== "system") return;

      // Check initial state
      setIsDark(isDarkMode());

      // Listen for system preference changes
      const mediaQuery = window.matchMedia("(prefers-color-scheme: dark)");
      const handleMediaChange = () => setIsDark(isDarkMode());
      mediaQuery.addEventListener("change", handleMediaChange);

      // Observe document changes (for site-level dark mode toggle)
      // Watch class, style (for color-scheme), and data attributes
      const observer = new MutationObserver(() => {
        setIsDark(isDarkMode());
      });
      observer.observe(document.documentElement, {
        attributes: true,
        attributeFilter: ["class", "style", "data-theme", "data-mode"],
      });

      return () => {
        mediaQuery.removeEventListener("change", handleMediaChange);
        observer.disconnect();
      };
    }, [theme]);

    // For IPython, detect cell magics and switch to appropriate language mode
    const langExtension = useMemo(() => {
      if (language === "ipython") {
        return getIPythonExtension(value).extension;
      }
      const ext = getLanguageExtension(language);
      return ext;
    }, [language, value]);

    // Determine which theme to use
    const themeExtension = useMemo(() => {
      if (theme === "light") return lightTheme;
      if (theme === "dark") return darkTheme;
      // "system" - use detected state
      return isDark ? darkTheme : lightTheme;
    }, [theme, isDark]);

    const extensions = useMemo(() => {
      const exts: Extension[] = [
        ...baseExtensions,
        langExtension,
        themeExtension,
      ];

      if (keyMap && keyMap.length > 0) {
        exts.unshift(keymap.of(keyMap));
      }

      if (placeholder) {
        exts.push(placeholderExt(placeholder));
      }

      if (lineWrapping) {
        exts.push(EditorView.lineWrapping);
      }

      if (readOnly) {
        exts.push(EditorView.editable.of(false));
      }

      if (additionalExtensions) {
        exts.push(...additionalExtensions);
      }

      return exts;
    }, [
      baseExtensions,
      langExtension,
      themeExtension,
      keyMap,
      placeholder,
      lineWrapping,
      readOnly,
      additionalExtensions,
    ]);

    const handleChange = useCallback(
      (val: string) => {
        onValueChange?.(val);
      },
      [onValueChange],
    );

    const handleFocus = useCallback(() => {
      onFocus?.();
    }, [onFocus]);

    const { setContainer, view } = useCodeMirror({
      container: editorRef.current,
      basicSetup: false,
      indentWithTab: false,
      extensions,
      maxHeight,
      value,
      onChange: handleChange,
      autoFocus,
    });

    // Store the editor view reference (kept for backwards compat with getEditor)
    useEffect(() => {
      editorViewRef.current = view || null;
    }, [view]);

    // Expose methods via ref - depends on `view` so handle updates when view is ready
    useImperativeHandle(
      ref,
      () => ({
        focus: () => {
          view?.focus();
        },
        setCursorPosition: (position: "start" | "end") => {
          if (view) {
            const doc = view.state.doc;
            const pos = position === "start" ? 0 : doc.length;
            view.dispatch({
              selection: { anchor: pos, head: pos },
              scrollIntoView: true,
            });
          }
        },
        getEditor: () => view || null,
      }),
      [view],
    );

    useEffect(() => {
      if (editorRef.current) {
        setContainer(editorRef.current);
      }
    }, [setContainer]);

    return (
      <div
        ref={editorRef}
        onBlur={onBlur}
        onFocus={handleFocus}
        className={cn("text-sm", className)}
      />
    );
  },
);

CodeMirrorEditor.displayName = "CodeMirrorEditor";

export default CodeMirrorEditor;
