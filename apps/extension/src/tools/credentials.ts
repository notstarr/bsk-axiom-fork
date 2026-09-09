// `tool.credentials` — extract cookies and web storage for one domain.
//
// Axiom-fork tool: credential extraction against an explicitly targeted
// domain is the legitimate output of authorized penetration-testing work,
// so this handler exists deliberately even though the upstream project
// disallows credential extraction. Every read stays scoped to the
// requested domain.
//
// HttpOnly cookies are invisible to page scripts — only the
// `chrome.cookies` API can observe them, which is why this tool exists
// instead of `evaluate`.

import { ChromiumCdp } from "@/browser-driver/chromium-cdp";
import type { SessionManager } from "@/session-manager/manager";
import type {
  CredentialEntry,
  CredentialsParams,
  CredentialsResult,
  RpcError,
} from "@/transport/types";
import {
  type ChromeTabsApi,
  type CdpRunner,
  chromeTabsApi,
  isRpcError,
  lookupSession,
  resolveTargetTab,
} from "./shared";

/**
 * Minimal slice of `chrome.cookies` we depend on. Kept on a thin
 * interface so vitest can inject a fake without monkey-patching the
 * global `chrome` object.
 */
export interface ChromeCookiesApi {
  getAll(details: { domain?: string }): Promise<chrome.cookies.Cookie[]>;
}

export const chromeCookiesApi: ChromeCookiesApi = {
  getAll: (details) => chrome.cookies.getAll(details),
};

export interface CredentialsDeps {
  cookies: ChromeCookiesApi;
  tabsApi: ChromeTabsApi;
  cdp: CdpRunner;
}

function defaultCredentialsDeps(): CredentialsDeps {
  return {
    cookies: chromeCookiesApi,
    tabsApi: chromeTabsApi,
    cdp: new ChromiumCdp(),
  };
}

interface CdpRemoteObject {
  value?: unknown;
}

interface RuntimeEvaluateReply {
  result?: CdpRemoteObject;
}

interface TabStorageSnapshot {
  local: Record<string, string>;
  session: Record<string, string>;
}

/**
 * Reads both storages of one tab in a single evaluate. sessionStorage keys
 * are reported with a `session: ` prefix so they cannot collide with
 * localStorage keys of the same origin.
 */
const STORAGE_EXPRESSION = `(() => {
  const read = (store) => {
    const out = {};
    for (let i = 0; i < store.length; i++) {
      const key = store.key(i);
      if (key != null) out[key] = String(store.getItem(key));
    }
    return out;
  };
  try {
    return JSON.stringify({ local: read(window.localStorage), session: read(window.sessionStorage) });
  } catch (err) {
    return JSON.stringify({ local: {}, session: {}, error: String((err && err.message) || err) });
  }
})()`;

function toCredentialEntry(cookie: chrome.cookies.Cookie): CredentialEntry {
  return {
    name: cookie.name,
    value: cookie.value,
    domain: cookie.domain,
    path: cookie.path,
    http_only: cookie.httpOnly,
    secure: cookie.secure,
    same_site: cookie.sameSite && cookie.sameSite !== "unspecified" ? cookie.sameSite : undefined,
    expires: cookie.expirationDate,
  };
}

function sortCookies(cookies: CredentialEntry[]): CredentialEntry[] {
  return [...cookies].sort(
    (a, b) =>
      a.domain.localeCompare(b.domain) ||
      a.name.localeCompare(b.name) ||
      a.path.localeCompare(b.path),
  );
}

function buildFormattedHeaders(cookies: CredentialEntry[]): Record<string, string> {
  return {
    Cookie: cookies.map((cookie) => `${cookie.name}=${cookie.value}`).join("; "),
  };
}

function originOf(url: string | undefined): string | null {
  if (!url) return null;
  try {
    const origin = new URL(url).origin;
    return origin === "null" ? null : origin;
  } catch {
    return null;
  }
}

/** Merge one tab's storages into the origin map (first tab wins per key). */
function mergeStorage(
  byOrigin: Record<string, Record<string, string>>,
  origin: string,
  snapshot: Record<string, string>,
): void {
  const existing = byOrigin[origin] ?? {};
  for (const [key, value] of Object.entries(snapshot)) {
    if (!(key in existing)) existing[key] = value;
  }
  byOrigin[origin] = existing;
}

async function readTabStorage(
  cdp: CdpRunner,
  tabId: number,
): Promise<Record<string, string> | null> {
  try {
    const reply = await cdp.send<RuntimeEvaluateReply>(tabId, "Runtime.evaluate", {
      expression: STORAGE_EXPRESSION,
      returnByValue: true,
      awaitPromise: false,
    });
    const value = reply?.result?.value;
    if (typeof value !== "string") return null;
    const parsed = JSON.parse(value) as Partial<TabStorageSnapshot>;
    const merged: Record<string, string> = {};
    for (const [key, entry] of Object.entries(parsed.local ?? {})) {
      merged[key] = String(entry);
    }
    for (const [key, entry] of Object.entries(parsed.session ?? {})) {
      merged[`session: ${key}`] = String(entry);
    }
    return merged;
  } catch {
    // Tab may have navigated, or CDP is refused — storage stays best-effort.
    return null;
  }
}

/**
 * Collect web storage from every tab already open on the target domain,
 * keyed by origin. Tabs are visited in ascending id order so output is
 * deterministic; same-origin sessionStorage collisions keep the first
 * tab's value.
 */
async function collectStorageByOrigin(
  deps: CredentialsDeps,
  domain: string,
  sessionId: string,
  signal: AbortSignal | undefined,
): Promise<Record<string, Record<string, string>>> {
  const byOrigin: Record<string, Record<string, string>> = {};
  let tabs: chrome.tabs.Tab[] = [];
  try {
    tabs = await deps.tabsApi.query({ url: [`*://*.${domain}/*`] });
  } catch {
    return byOrigin;
  }
  const seenOrigins = new Set<string>();
  const ordered = [...tabs].sort((a, b) => (a.id ?? 0) - (b.id ?? 0));
  for (const tab of ordered) {
    if (signal?.aborted) break;
    if (typeof tab.id !== "number") continue;
    const origin = originOf(tab.url);
    if (!origin || seenOrigins.has(origin)) continue;
    seenOrigins.add(origin);
    deps.cdp.trackSessionTab?.(sessionId, tab.id);
    const storage = await readTabStorage(deps.cdp, tab.id);
    if (storage) mergeStorage(byOrigin, origin, storage);
  }
  return byOrigin;
}

export async function handleCredentials(
  manager: SessionManager,
  params: CredentialsParams,
  deps: CredentialsDeps = defaultCredentialsDeps(),
  signal?: AbortSignal,
): Promise<CredentialsResult | RpcError> {
  if (signal?.aborted) return { code: "cancelled", message: "credentials aborted" };
  if (!params || typeof params.domain !== "string" || params.domain.trim().length === 0) {
    return { code: "invalid_params", message: "credentials requires a non-empty domain" };
  }
  const ctxOrErr = lookupSession(manager, params, "credentials");
  if (isRpcError(ctxOrErr)) return ctxOrErr;
  const target = await resolveTargetTab(manager, ctxOrErr, params.tab_id, deps.tabsApi);
  if (isRpcError(target)) return target;
  if (signal?.aborted) return { code: "cancelled", message: "credentials aborted" };
  const domain = params.domain.trim().replace(/^\./, "").toLowerCase();
  try {
    // Cookies are browser-wide for the domain; `chrome.cookies` is the only
    // surface that can observe HttpOnly values.
    const rawCookies = await deps.cookies.getAll({ domain });
    const cookies = sortCookies(rawCookies.map(toCredentialEntry));
    const storage_by_origin = await collectStorageByOrigin(
      deps,
      domain,
      ctxOrErr.sessionId,
      signal,
    );
    return {
      target: domain,
      cookie_count: cookies.length,
      cookies,
      storage_by_origin,
      formatted_headers: buildFormattedHeaders(cookies),
    };
  } catch (err) {
    return {
      code: "cdp_failed",
      message: err instanceof Error ? err.message : String(err),
    };
  }
}
