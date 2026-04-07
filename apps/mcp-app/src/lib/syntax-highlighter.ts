/**
 * PrismLight syntax highlighter with selective language registration.
 * Only includes languages relevant to notebook source cells.
 */

import PrismLight from "react-syntax-highlighter/dist/esm/prism-light";
import {
  oneDark,
  oneLight,
} from "react-syntax-highlighter/dist/esm/styles/prism";

import bash from "react-syntax-highlighter/dist/esm/languages/prism/bash";
import python from "react-syntax-highlighter/dist/esm/languages/prism/python";
import typescript from "react-syntax-highlighter/dist/esm/languages/prism/typescript";

PrismLight.registerLanguage("python", python);
PrismLight.registerLanguage("py", python);
PrismLight.registerLanguage("bash", bash);
PrismLight.registerLanguage("shell", bash);
PrismLight.registerLanguage("typescript", typescript);
PrismLight.registerLanguage("ts", typescript);

export { PrismLight as SyntaxHighlighter, oneDark, oneLight };
