"use client";

import { ReactNode } from "react";
import { useAuthGate, useAuth } from "@/lib/hooks/useAuth";

export function AuthGate({ children }: { children: ReactNode }) {
  useAuthGate();
  const { authed, ready } = useAuth();
  if (!ready || !authed) {
    return (
      <div className="nd flex min-h-dvh items-center justify-center bg-nd-bg">
        <p className="nd-aux" role="status">Authenticating…</p>
      </div>
    );
  }
  return <>{children}</>;
}
