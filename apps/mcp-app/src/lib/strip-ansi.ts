/** Strip ANSI escape codes from text */
export function stripAnsi(text: string): string {
  return text.replace(/\x1b\[[0-9;]*[A-Za-z]|\x1b\].*?\x07|\x1b\(B/g, "");
}
