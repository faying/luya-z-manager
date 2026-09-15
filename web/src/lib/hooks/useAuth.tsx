"use client";

import { createContext, useCallback, useContext, useEffect, useState, ReactNode } from "react";
import { useRouter, usePathname } from "next/navigation";
import { getToken, setToken, UNAUTHORIZED_EVENT } from "@/lib/api/client";
import { login as apiLogin, logout as apiLogout } from "@/lib/api/auth";

type AuthCtx = {
  authed: boolean;
  ready: boolean;
  login: (password: string) => Promise<void>;
  logout: () => void;
};

const Ctx = createContext<AuthCtx | null>(null);

export function AuthProvider({ children }: { children: ReactNode }) {
  const [authed, setAuthed] = useState(false);
  const [ready, setReady] = useState(false);

  useEffect(() => {
    setAuthed(!!getToken());
    setReady(true);
    // A 401 anywhere (expired/invalid token) clears the session and fires this
    // event — flip to unauthed so <AuthGate> redirects to login automatically,
    // instead of leaving the page stuck showing "unauthorized" until a refresh.
    const onUnauthorized = () => setAuthed(false);
    window.addEventListener(UNAUTHORIZED_EVENT, onUnauthorized);
    return () => window.removeEventListener(UNAUTHORIZED_EVENT, onUnauthorized);
  }, []);

  const login = useCallback(async (password: string) => {
    await apiLogin(password);
    setAuthed(true);
  }, []);

  const logout = useCallback(() => {
    apiLogout();
    setAuthed(false);
  }, []);

  return <Ctx.Provider value={{ authed, ready, login, logout }}>{children}</Ctx.Provider>;
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
