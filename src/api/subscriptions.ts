/**
 * Subscription API — typed wrappers around all subscription-related
 * Tauri backend commands.
 *
 * If the backend renames a command or changes its parameter shape,
 * only the function body below needs to change. All callers throughout
 * the app are insulated.
 */

import { apiCall } from "./client";
import type { Subscription, NewSubscriptionInput } from "./types";

export const subscriptionsApi = {
  /** Get all subscriptions. */
  list(): Promise<Subscription[]> {
    return apiCall<Subscription[]>("list_subscriptions");
  },

  /** Get a single subscription by ID. */
  get(id: number): Promise<Subscription> {
    return apiCall<Subscription>("get_subscription", { id });
  },

  /** Add a new subscription. */
  create(input: NewSubscriptionInput): Promise<Subscription> {
    return apiCall<Subscription>("add_subscription", input);
  },

  /** Update an existing subscription. */
  update(
    id: number,
    input: Partial<NewSubscriptionInput>,
  ): Promise<Subscription> {
    return apiCall<Subscription>("update_subscription", { id, ...input });
  },

  /** Remove a subscription. */
  remove(id: number): Promise<void> {
    return apiCall<void>("remove_subscription", { id });
  },

  /** Toggle the use_website flag. */
  toggleUseWebsite(id: number): Promise<Subscription> {
    return apiCall<Subscription>("toggle_use_website", { id });
  },

  /** Toggle the auto_classify flag. */
  toggleAutoClassify(id: number): Promise<Subscription> {
    return apiCall<Subscription>("toggle_auto_classify", { id });
  },
};
