import type { TerminalRunResult } from "./runResult";
export const CONSOLE_COMMAND_LIMIT = 64 * 1024;
const encoder = new TextEncoder();
export function boundedCommand(command: string): boolean {
  return command.length <= CONSOLE_COMMAND_LIMIT && encoder.encode(command).length <= CONSOLE_COMMAND_LIMIT;
}
export function retainCommand(history: string[], command: string): string[] {
  if (history.at(-1) === command) return history;
  return [...history.slice(-99), command];
}
export function retainExecutions<T extends { command: string; error?: string; result?: TerminalRunResult }>(entries: T[]): T[] {
  let bytes = 0;
  const retained: T[] = [];
  for (let i = entries.length - 1; i >= 0 && retained.length < 100; i--) {
    const entry = entries[i];
    bytes += encoder.encode(entry.command + (entry.error ?? "") + (entry.result?.stdout_tail ?? "") + (entry.result?.stderr_tail ?? "")).length;
    if (bytes > 8 * 1024 * 1024) break;
    retained.push(entry);
  }
  return retained.reverse();
}
export function followsConsoleOutput(scrollTop: number, clientHeight: number, scrollHeight: number): boolean {
  return scrollHeight - clientHeight - scrollTop <= 24;
}
