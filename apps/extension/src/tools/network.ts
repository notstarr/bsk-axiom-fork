import { ChromiumCdp } from "@/browser-driver/chromium-cdp";
import type { SessionManager } from "@/session-manager/manager";
import type { NetworkParams, NetworkResult, RpcError } from "@/transport/types";
import {
  type ChromeTabsApi,
  chromeTabsApi,
  isRpcError,
  lookupSession,
  parseBufferedReadBounds,
  resolveTargetTab,
} from "./shared";

export interface NetworkCdpRunner {
  trackSessionTab?(sessionId: string, tabId: number): void;
  ensureNetworkCapture(tabId: number): Promise<void>;
  networkEntriesSince(
    tabId: number,
    since: number | undefined,
    limit: number,
    maxTextChars: number,
    urlFilter?: string,
    includeStatic?: boolean,
  ): NetworkResult;
  networkEntryDetail?(tabId: number, sequence: number): NetworkResult;
  clearNetworkBuffer?(tabId: number): NetworkResult;
}

export interface NetworkDeps {
  cdp: NetworkCdpRunner;
  tabsApi: ChromeTabsApi;
}

function defaultNetworkDeps(): NetworkDeps {
  return {
    cdp: new ChromiumCdp(),
    tabsApi: chromeTabsApi,
  };
}

export async function handleNetwork(
  manager: SessionManager,
  params: NetworkParams,
  deps: NetworkDeps = defaultNetworkDeps(),
  signal?: AbortSignal,
): Promise<NetworkResult | RpcError> {
  if (signal?.aborted) return { code: "cancelled", message: "network aborted" };
  const ctxOrErr = lookupSession(manager, params, "network");
  if (isRpcError(ctxOrErr)) return ctxOrErr;
  const bounds = parseBufferedReadBounds(params);
  if (isRpcError(bounds)) return bounds;
  const target = await resolveTargetTab(manager, ctxOrErr, params.tab_id, deps.tabsApi);
  if (isRpcError(target)) return target;
  if (params.clear === true && params.detail !== undefined) {
    return {
      code: "invalid_params",
      message: "clear and detail are mutually exclusive",
    };
  }
  if (params.detail !== undefined && (!Number.isSafeInteger(params.detail) || params.detail <= 0)) {
    return { code: "invalid_params", message: "detail must be a positive sequence" };
  }

  try {
    if (signal?.aborted) return { code: "cancelled", message: "network aborted" };
    deps.cdp.trackSessionTab?.(ctxOrErr.sessionId, target.tabId);
    await deps.cdp.ensureNetworkCapture(target.tabId);
    if (signal?.aborted) return { code: "cancelled", message: "network aborted" };
    if (params.clear === true) {
      if (!deps.cdp.clearNetworkBuffer) {
        return { code: "cdp_failed", message: "clear unsupported by this runner" };
      }
      return deps.cdp.clearNetworkBuffer(target.tabId);
    }
    if (params.detail !== undefined) {
      if (!deps.cdp.networkEntryDetail) {
        return { code: "cdp_failed", message: "detail unsupported by this runner" };
      }
      return deps.cdp.networkEntryDetail(target.tabId, params.detail);
    }
    return deps.cdp.networkEntriesSince(
      target.tabId,
      bounds.since,
      bounds.limit,
      bounds.maxTextChars,
      params.url_filter,
      params.include_static === true,
    );
  } catch (err) {
    return {
      code: "cdp_failed",
      message: err instanceof Error ? err.message : String(err),
    };
  }
}
