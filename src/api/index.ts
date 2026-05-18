/**
 * API client for the RSS reader backend.
 *
 * All backend communication goes through this layer. When backend
 * commands are renamed or restructured, only files in this directory
 * need updating — the rest of the application is insulated.
 */

export { apiCall, AppApiError } from "./client";
export type { Subscription, NewSubscriptionInput, UpdateSubscriptionInput, FeedItem, AiClassificationResponse, AiConfig, ImportResult, FetchProgress, FetchSuccess, FetchError, TranslationProgress, TranslationError } from "./types";
export { subscriptionsApi } from "./subscriptions";
