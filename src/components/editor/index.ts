export {
  CodeMirrorEditor,
  type CodeMirrorEditorProps,
  type CodeMirrorEditorRef,
} from "./codemirror-editor";
export {
  coreSetup,
  defaultExtensions,
  minimalExtensions,
  minimalSetup,
  notebookEditorTheme,
} from "./extensions";
export {
  CELL_MAGIC_LANGUAGES,
  detectCellMagic,
  getCellMagicLanguage,
  ipythonHighlighting,
  ipythonIndent,
  ipythonStyles,
  ipythonStylesDark,
} from "./ipython";
export {
  detectLanguage,
  fileExtensionToLanguage,
  getIPythonExtension,
  getLanguageExtension,
  languageDisplayNames,
  type SupportedLanguage,
} from "./languages";
export { searchHighlight } from "./search-highlight";
export {
  darkTheme,
  documentHasDarkMode,
  getAutoTheme,
  getTheme,
  isDarkMode,
  lightTheme,
  prefersDarkMode,
  type ThemeMode,
} from "./themes";
