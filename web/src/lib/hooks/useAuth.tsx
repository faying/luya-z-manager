"use client";

import { createContext, useCallback, useContext, useEffect, useState, ReactNode } from "react";
import { useRouter, usePathname } from "next/navigation";
import { getToken, LOGIN_EVENT, setToken, UNAUTHORIZED_EVENT } from "@/lib/api/client";
import { login as apiLogin, logout as apiLogout } from "@/lib/api/auth";

type AuthCtx = {
  authed: boolean;
  ready: boolean;
  /** The session expired mid-use: show the in-place login dialog, keep the page. */
  expired: boolean;
  login: (password: string) => Promise<void>;
  logout: () => void;
};

const Ctx = createContext<AuthCtx | null>(null);

export function AuthProvider({ children }: { children: ReactNode }) {
  const [authed, setAuthed] = useState(false);
  const [ready, setReady] = useState(false);
  const [expired, setExpired] = useState(false);

  useEffect(() => {
    // The token lives in localStorage, which the static export can't read at
    // build time, so it is picked up once after mount.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setAuthed(!!getToken());
    setReady(true);
    // A 401 on the current token (see apiFetch) clears it and fires this.
    // Keep the page mounted — forms and pending writes survive — and ask for
    // the password in place (design doc §5.1). A fresh token fires
    // LOGIN_EVENT, which closes the dialog and lets waiting requests resend.
    const onUnauthorized = () => setExpired(true);
    const onLogin = () => setExpired(false);
    window.addEventListener(UNAUTHORIZED_EVENT, onUnauthorized);
    window.addEventListener(LOGIN_EVENT, onLogin);
    return () => {
      window.removeEventListener(UNAUTHORIZED_EVENT, onUnauthorized);
      window.removeEventListener(LOGIN_EVENT, onLogin);
    };
  }, []);

  const login = useCallback(async (password: string) => {
    await apiLogin(password);
    setAuthed(true);
  }, []);

  const logout = useCallback(() => {
    apiLogout();
    setExpired(false);
    setAuthed(false);
  }, []);

  return <Ctx.Provider value={{ authed, ready, expired, login, logout }}>{children}</Ctx.Provider>;
}

export function useAuth(): AuthCtx {
  const c = useContext(Ctx);
  if (!c) throw new Error("useAuth must be used inside <AuthProvider>");
  return c;
}

const ADMIN_BASE = "/";
const LOGIN_PATH = "/login";

function stripSlash(p: string | null): string {
  if (!p) return "";
  return p.length > 1 && p.endsWith("/") ? p.slice(0, -1) : p;
}

export function useAuthGate() {
  const { authed, ready } = useAuth();
  const router = useRouter();
  const pathname = stripSlash(usePathname());
  useEffect(() => {
    if (!ready) return;
    const onLogin = pathname === LOGIN_PATH;
    if (!authed && !onLogin) {
      const redirect = pathname.startsWith(ADMIN_BASE) ? `?next=${encodeURIComponent(pathname)}` : "";
      router.replace(`${LOGIN_PATH}${redirect}`);
    } else if (authed && onLogin) {
      router.replace(ADMIN_BASE);
    }
  }, [authed, ready, pathname, router]);
}

export function setTokenManual(token: string | null) {
  setToken(token);
}
