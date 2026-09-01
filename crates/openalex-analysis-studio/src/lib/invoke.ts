import { invoke as tauriInvoke } from '@tauri-apps/api/core';

export function invoke<T = unknown>(command: string, args?: Record<string, unknown>): Promise<T> {
  return tauriInvoke<T>(command, args);
}

export function call<T = unknown>(command: string, args?: Record<string, unknown>): Promise<T> {
  return invoke<T>(command, args);
}
