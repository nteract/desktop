/** Strip ANSI escape codes from text */
export function stripAnsi(text: string): string {
  // eslint-disable-next-line no-control-regex -- ANSI escape sequences use control characters
  return text.replace(/\x1b\[[0-9;]*[A-Za-z]|\x1b\].*?\x07|\x1b\(B/g, "");
}
