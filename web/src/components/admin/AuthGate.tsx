"use client";

import { ReactNode } from "react";
import { useAuthGate, useAuth } from "@/lib/hooks/useAuth";

export function AuthGate({ children }: { children: ReactNode }) {
  useAuthGate();
  const { authed, ready } = useAuth();
  if (!ready || !authed) {
    return (
      <div className="flex min-h-screen items-center justify-center bg-bg">
        <div className="admin-status text-text-dim">Authenticating</div>
      </div>
    );
  }
  return <>{children}</>;
}
