import { Annotation, type Extension, Compartment } from "@codemirror/state";
import {
  EditorView,
  type KeyBinding,
  keymap,
  placeholder as placeholderExt,
} from "@codemirror/view";
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

/**
 * Annotation marking a transaction as an external (inbound) change.
 * The editor's updateListener skips the onValueChange callback for these.
 * The CRDT bridge annotates reconcile transactions with this.
 */
export const externalChangeAnnotation = Annotation.define<boolean>();

export interface CodeMirrorEditorProps {
  /** Initial editor content (used on mount; ongoing sync handled by CRDT bridge extension) */
  initialValue?: string;
  /** Language for syntax highlighting */
  language?: SupportedLanguage;
  /** Callback when content changes (for non-CRDT consumers; CRDT bridge handles sync directly) */
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
 * CodeMirror 6 editor component for notebook cells.
 *
 * Manages an EditorView directly — no wrapper library. The editor is
 * **uncontrolled**: `initialValue` sets content on mount; ongoing source
 * sync is handled by the CRDT bridge extension (passed in `extensions`).
 *
 * The optional `onValueChange` callback fires on every local document
 * change for consumers that need it (e.g., non-CRDT editors). It does
 * NOT fire for inbound CRDT changes (reconcile-annotated transactions).
 */
export const CodeMirrorEditor = forwardRef<
  CodeMirrorEditorRef,
  CodeMirrorEditorProps
>(
  (
    {
      initialValue = "",
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
    const containerRef = useRef<HTMLDivElement | null>(null);
    const viewRef = useRef<EditorView | null>(null);

    // Stable refs for callbacks so the updateListener closure doesn't go stale.
    const onValueChangeRef = useRef(onValueChange);
    onValueChangeRef.current = onValueChange;

    // Compartments for dynamic reconfiguration without recreating the view.
    const langCompartment = useRef(new Compartment());
    const themeCompartment = useRef(new Compartment());
    const keymapCompartment = useRef(new Compartment());
    const placeholderCompartment = useRef(new Compartment());
    const lineWrappingCompartment = useRef(new Compartment());
    const readOnlyCompartment = useRef(new Compartment());
    const additionalCompartment = useRef(new Compartment());

    // Track dark mode state for "system" theme
    const [isDark, setIsDark] = useState(() =>
      typeof window !== "undefined" ? isDarkMode() : false,
    );

    // Listen for dark mode changes (system preference + document class)
    useEffect(() => {
      if (theme !== "system") return;

      setIsDark(isDarkMode());

      const mediaQuery = window.matchMedia("(prefers-color-scheme: dark)");
      const handleMediaChange = () => setIsDark(isDarkMode());
      mediaQuery.addEventListener("change", handleMediaChange);

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

    // ── Language extension ────────────────────────────────────────────
    //
    // For IPython cells, magic detection (%%bash, %%sql, etc.) must
    // re-run when the document changes — not just on mount. We compute
    // the initial extension here, then reconfigure dynamically via an
    // updateListener added to the EditorView (see the mount effect).

    const langExtension = useMemo(() => {
      if (language === "ipython") {
        return getIPythonExtension(initialValue).extension;
      }
      return getLanguageExtension(language);
    }, [language, initialValue]);

    // Track the last-detected magic so we only reconfigure when it changes.
    const lastMagicRef = useRef<string | null>(null);
    // Store the language prop in a ref so the updateListener closure stays fresh.
    const languageRef = useRef(language);
    languageRef.current = language;

    // ── Theme extension ──────────────────────────────────────────────

    const themeExtension = useMemo(() => {
      if (theme === "light") return lightTheme;
      if (theme === "dark") return darkTheme;
      return isDark ? darkTheme : lightTheme;
    }, [theme, isDark]);

    // ── Max height ───────────────────────────────────────────────────

    const maxHeightTheme = useMemo((): Extension[] => {
      if (!maxHeight) return [];
      return [
        EditorView.theme({
          "&": { maxHeight },
          ".cm-scroller": { overflow: "auto" },
        }),
      ];
    }, [maxHeight]);

    // ── Create EditorView on mount ───────────────────────────────────

    useEffect(() => {
      const container = containerRef.current;
      if (!container) return;

      const updateListener = EditorView.updateListener.of((vu) => {
        if (!vu.docChanged) return;

        // Fire onValueChange only for local edits — skip transactions
        // annotated as external (inbound CRDT reconcile changes).
        const hasLocalEdit = vu.transactions.some(
          (tr) => tr.docChanged && !tr.annotation(externalChangeAnnotation),
        );

        if (hasLocalEdit && onValueChangeRef.current) {
          onValueChangeRef.current(vu.state.doc.toString());
        }

        // ── IPython magic re-detection ─────────────────────────────
        // When the language is "ipython", check if the first line changed
        // to a different cell magic and reconfigure the language extension.
        if (languageRef.current === "ipython") {
          // Only read the first line — magic detection is O(1) vs O(n) toString().
          const firstLine = vu.state.doc.line(1).text;
          const { extension: newLangExt, cellMagic } =
            getIPythonExtension(firstLine);
          const magicKey = cellMagic ?? "";
          if (magicKey !== (lastMagicRef.current ?? "")) {
            lastMagicRef.current = magicKey;
            vu.view.dispatch({
              effects: langCompartment.current.reconfigure(newLangExt),
            });
          }
        }
      });

      const view = new EditorView({
        doc: initialValue,
        extensions: [
          // Custom keymaps first — highest precedence (Shift-Enter, etc.)
          keymapCompartment.current.of(
            keyMap && keyMap.length > 0 ? keymap.of(keyMap) : [],
          ),
          ...baseExtensions,
          langCompartment.current.of(langExtension),
          themeCompartment.current.of(themeExtension),
          placeholderCompartment.current.of(
            placeholder ? placeholderExt(placeholder) : [],
          ),
          lineWrappingCompartment.current.of(
            lineWrapping ? EditorView.lineWrapping : [],
          ),
          readOnlyCompartment.current.of(
            readOnly ? EditorView.editable.of(false) : [],
          ),
          additionalCompartment.current.of(additionalExtensions ?? []),
          ...maxHeightTheme,
          updateListener,
        ],
        parent: container,
      });

      viewRef.current = view;

      // Toggling the placeholder forces a decoration change that triggers
      // updateInner(), rebuilding the line tiles. Without this, the initial
      // tile DOM renders a few pixels too tall — CM's measure cycle alone
      // won't call updateInner() when the viewport hasn't changed.
      requestAnimationFrame(() => {
        if (placeholder) {
          view.dispatch({
            effects: placeholderCompartment.current.reconfigure([]),
          });
          view.dispatch({
            effects: placeholderCompartment.current.reconfigure(
              placeholderExt(placeholder),
            ),
          });
        }
        if (autoFocus) {
          view.focus();
        }
      });

      return () => {
        viewRef.current = null;
        view.destroy();
      };
      // EditorView is created once on mount. Dynamic props are reconfigured
      // via compartments in the effects below.
      // eslint-disable-next-line react-hooks/exhaustive-deps
    }, []);

    // Editor focus is handled explicitly:
    //  - On mount: autoFocus prop (above) focuses via requestAnimationFrame
    //  - Keyboard nav: focusCell() from useEditorRegistry calls view.focus()
    //  - Mouse clicks: native DOM focus on the editor element
    // No post-mount autoFocus effect needed — it caused the editor to steal
    // focus from outputs/iframes whenever isFocused toggled.

    // ── Dynamic reconfiguration via compartments ─────────────────────

    useEffect(() => {
      viewRef.current?.dispatch({
        effects: langCompartment.current.reconfigure(langExtension),
      });
    }, [langExtension]);

    useEffect(() => {
      viewRef.current?.dispatch({
        effects: themeCompartment.current.reconfigure(themeExtension),
      });
    }, [themeExtension]);

    useEffect(() => {
      viewRef.current?.dispatch({
        effects: keymapCompartment.current.reconfigure(
          keyMap && keyMap.length > 0 ? keymap.of(keyMap) : [],
        ),
      });
    }, [keyMap]);

    useEffect(() => {
      viewRef.current?.dispatch({
        effects: placeholderCompartment.current.reconfigure(
          placeholder ? placeholderExt(placeholder) : [],
        ),
      });
    }, [placeholder]);

    useEffect(() => {
      viewRef.current?.dispatch({
        effects: lineWrappingCompartment.current.reconfigure(
          lineWrapping ? EditorView.lineWrapping : [],
        ),
      });
    }, [lineWrapping]);

    useEffect(() => {
      viewRef.current?.dispatch({
        effects: readOnlyCompartment.current.reconfigure(
          readOnly ? EditorView.editable.of(false) : [],
        ),
      });
    }, [readOnly]);

    useEffect(() => {
      viewRef.current?.dispatch({
        effects: additionalCompartment.current.reconfigure(
          additionalExtensions ?? [],
        ),
      });
    }, [additionalExtensions]);

    // ── Imperative handle ────────────────────────────────────────────

    useImperativeHandle(
      ref,
      () => ({
        focus: () => {
          viewRef.current?.focus();
        },
        setCursorPosition: (position: "start" | "end") => {
          const view = viewRef.current;
          if (view) {
            const pos = position === "start" ? 0 : view.state.doc.length;
            view.dispatch({
              selection: { anchor: pos, head: pos },
              scrollIntoView: true,
            });
          }
        },
        getEditor: () => viewRef.current,
      }),
      [],
    );

    // ── Focus / blur via DOM events on the wrapper ───────────────────

    const handleFocus = useCallback(() => {
      onFocus?.();
    }, [onFocus]);

    return (
      <div
        ref={containerRef}
        onBlur={onBlur}
        onFocus={handleFocus}
        className={cn("text-sm", className)}
      />
    );
  },
);

CodeMirrorEditor.displayName = "CodeMirrorEditor";

export default CodeMirrorEditor;
