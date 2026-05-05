/**
 * Typed invoke wrapper for Tauri backend commands.
 *
 * All IPC calls go through this module. If the backend renames a command,
 * changes parameter shapes, or restructures error responses, ONLY this
 * module needs to change — every caller gets the fix automatically.
 *
 * Usage:
 *   import { apiCall } from "./api/client";
 *   const items = await apiCall<FeedItem[]>("get_items", { subscriptionId: 1 });
 */

import { invoke } from "@tauri-apps/api/core";

/**
 * Wraps Tauri's invoke() with unified error handling.
 * Backend errors (which arrive as strings) are wrapped in typed AppApiError
 * so callers can distinguish "operation failed" from "network error" etc.
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export async function apiCall<T>(command: string, args?: Record<string, any>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    throw new AppApiError(
      typeof error === "string" ? error : "Unknown backend error",
      command,
    );
  }
}

/**
 * Typed error returned by all API functions.
 * The `command` field identifies which backend command failed,
 * making debugging and error reporting easier.
 */
export class AppApiError extends Error {
  constructor(
    message: string,
    public readonly command: string,
  ) {
    super(message);
    this.name = "AppApiError";
  }
}
