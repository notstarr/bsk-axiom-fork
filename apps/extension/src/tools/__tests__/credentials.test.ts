import { describe, expect, it, vi } from "vitest";
import { SessionManager } from "@/session-manager/manager";
import type { CredentialsResult } from "@/transport/types";
import { handleCredentials, type CredentialsDeps } from "../credentials";

function fakeAgentWindow(ids: number[]) {
  let i = 0;
  return {
    create: vi.fn(async () => {
      const id = ids[i++];
      if (id === undefined) throw new Error("ran out of fake ids");
      return { windowId: id, initialTabIds: [] };
    }),
    remove: vi.fn(async () => {}),
    ensureActiveTab: vi.fn(async () => 1),
  };
}

function cookie(overrides: Partial<chrome.cookies.Cookie> = {}): chrome.cookies.Cookie {
  return {
    name: "sid",
    value: "abc",
    domain: ".example.com",
    path: "/",
    secure: true,
    httpOnly: true,
    sameSite: "lax",
    session: false,
    hostOnly: false,
    ...overrides,
  } as chrome.cookies.Cookie;
}

function makeDeps(
  opts: {
    cookies?: chrome.cookies.Cookie[];
    getAllError?: Error;
    tabs?: chrome.tabs.Tab[];
    storageReply?: unknown;
    sendError?: Error;
    get?: (tabId: number) => Promise<chrome.tabs.Tab>;
  } = {},
) {
  const getAll = vi.fn(async () => {
    if (opts.getAllError) throw opts.getAllError;
    return opts.cookies ?? [];
  });
  const send = vi.fn(async (_tabId: number, _method: string, _params?: object) => {
    if (opts.sendError) throw opts.sendError;
    return opts.storageReply ?? { result: { value: JSON.stringify({ local: {}, session: {} }) } };
  });
  const trackSessionTab = vi.fn();
  const cdp = {
    send: send as unknown as CredentialsDeps["cdp"]["send"],
    trackSessionTab,
  } satisfies CredentialsDeps["cdp"];
  const tabsApi = {
    get:
      opts.get ??
      vi.fn(
        async (tabId: number) => ({ id: tabId, windowId: 100, active: true }) as chrome.tabs.Tab,
      ),
    query: vi.fn(async (q: chrome.tabs.QueryInfo) => {
      // URL-pattern queries are the handler's domain-tab sweep; everything
      // else is resolveTargetTab's active-tab lookup in the Agent Window.
      if (q.url) return opts.tabs ?? [];
      if (typeof q.windowId === "number") {
        return [{ id: 7, windowId: q.windowId, active: true } as chrome.tabs.Tab];
      }
      return [];
    }),
  };
  const deps: CredentialsDeps = { cookies: { getAll }, tabsApi, cdp };
  return { deps, getAll, send, trackSessionTab };
}

const NO_STORAGE = { result: { value: JSON.stringify({ local: {}, session: {} }) } };

describe("handleCredentials", () => {
  it("returns sorted cookies, the Cookie header, and domain storage", async () => {
    const sm = new SessionManager({ agentWindow: fakeAgentWindow([100]) });
    await sm.start("aa11");
    const { deps, getAll, send, trackSessionTab } = makeDeps({
      cookies: [
        cookie({ name: "zz_late", httpOnly: false }),
        cookie({ name: "aa_session", domain: ".example.com" }),
        cookie({ name: "mid", domain: "sub.example.com", httpOnly: false, sameSite: "unspecified" }),
      ],
      tabs: [
        { id: 3, url: "https://app.example.com/inbox" } as chrome.tabs.Tab,
        { id: 2, url: "https://app.example.com/other" } as chrome.tabs.Tab,
      ],
      storageReply: {
        result: {
          value: JSON.stringify({
            local: { theme: "dark", token: "t-2" },
            session: { otp: "42" },
          }),
        },
      },
    });

    const res = (await handleCredentials(sm, { session_id: "aa11", domain: "example.com" }, deps)) as
      | CredentialsResult
      | { code: string };
    if ("code" in res) throw new Error(`unexpected error: ${JSON.stringify(res)}`);

    expect(getAll).toHaveBeenCalledWith({ domain: "example.com" });
    expect(res.target).toBe("example.com");
    expect(res.cookie_count).toBe(3);
    // Sorted by domain then name: .example.com entries before sub.example.com.
    expect(res.cookies.map((c) => c.name)).toEqual(["aa_session", "zz_late", "mid"]);
    expect(res.cookies[0].http_only).toBe(true);
    // unspecified SameSite collapses to absent, mirroring the protocol Option.
    expect(res.cookies[2].same_site).toBeUndefined();
    expect(res.formatted_headers.Cookie).toBe("aa_session=abc; zz_late=abc; mid=abc");
    // Lower tab id (2) wins for the same origin.
    expect(res.storage_by_origin).toEqual({
      "https://app.example.com": { theme: "dark", token: "t-2", "session: otp": "42" },
    });
    expect(send).toHaveBeenCalledTimes(1);
    expect(trackSessionTab).toHaveBeenCalledWith("aa11", 2);
  });

  it("normalises a leading-dot domain", async () => {
    const sm = new SessionManager({ agentWindow: fakeAgentWindow([100]) });
    await sm.start("aa11");
    const { deps, getAll } = makeDeps();

    const res = await handleCredentials(sm, { session_id: "aa11", domain: ".Example.com" }, deps);

    expect(getAll).toHaveBeenCalledWith({ domain: "example.com" });
    expect(res).toMatchObject({ target: "example.com", cookie_count: 0 });
    expect(res).toMatchObject({ formatted_headers: { Cookie: "" } });
  });

  it("refuses an empty domain", async () => {
    const sm = new SessionManager({ agentWindow: fakeAgentWindow([100]) });
    await sm.start("aa11");
    const { deps } = makeDeps();

    const res = await handleCredentials(sm, { session_id: "aa11", domain: "   " }, deps);

    expect(res).toEqual({ code: "invalid_params", message: "credentials requires a non-empty domain" });
  });

  it("surfaces cookie store failures as cdp_failed", async () => {
    const sm = new SessionManager({ agentWindow: fakeAgentWindow([100]) });
    await sm.start("aa11");
    const { deps } = makeDeps({ getAllError: new Error("cookies permission denied") });

    const res = await handleCredentials(sm, { session_id: "aa11", domain: "example.com" }, deps);

    expect(res).toEqual({ code: "cdp_failed", message: "cookies permission denied" });
  });

  it("keeps cookies when storage reads fail", async () => {
    const sm = new SessionManager({ agentWindow: fakeAgentWindow([100]) });
    await sm.start("aa11");
    const { deps, send } = makeDeps({
      cookies: [cookie()],
      tabs: [{ id: 5, url: "https://app.example.com/" } as chrome.tabs.Tab],
      sendError: new Error("target detached"),
    });

    const res = (await handleCredentials(sm, { session_id: "aa11", domain: "example.com" }, deps)) as
      | CredentialsResult
      | { code: string };
    if ("code" in res) throw new Error(`unexpected error: ${JSON.stringify(res)}`);

    expect(send).toHaveBeenCalledTimes(1);
    expect(res.cookie_count).toBe(1);
    expect(res.storage_by_origin).toEqual({});
  });

  it("skips tabs without usable ids or parseable origins", async () => {
    const sm = new SessionManager({ agentWindow: fakeAgentWindow([100]) });
    await sm.start("aa11");
    const { deps, send } = makeDeps({
      tabs: [
        { url: "https://app.example.com/" } as chrome.tabs.Tab,
        { id: 6, url: "not a url" } as chrome.tabs.Tab,
      ],
      storageReply: NO_STORAGE,
    });

    const res = (await handleCredentials(sm, { session_id: "aa11", domain: "example.com" }, deps)) as
      | CredentialsResult
      | { code: string };
    if ("code" in res) throw new Error(`unexpected error: ${JSON.stringify(res)}`);

    expect(send).not.toHaveBeenCalled();
    expect(res.storage_by_origin).toEqual({});
  });

  it("hides other sessions' Agent Window tabs", async () => {
    const sm = new SessionManager({ agentWindow: fakeAgentWindow([100, 101]) });
    await sm.start("aa11");
    await sm.start("bb22");
    const { deps } = makeDeps({
      get: vi.fn(async () => ({ id: 9, windowId: 101, active: true }) as chrome.tabs.Tab),
    });

    const res = await handleCredentials(
      sm,
      { session_id: "aa11", tab_id: 9, domain: "example.com" },
      deps,
    );

    expect(res).toMatchObject({ code: "not_found" });
  });
});
