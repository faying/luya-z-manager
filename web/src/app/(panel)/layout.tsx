"use client";

import { usePathname } from "next/navigation";
import { AuthProvider } from "@/lib/hooks/useAuth";
import { AuthGate } from "@/components/admin/AuthGate";
import { AdminShell } from "@/components/admin/AdminShell";
import { I18nProvider } from "@/components/admin/I18nProvider";
import "./admin.css";

export default function AdminLayout({ children }: { children: React.ReactNode }) {
  const pathname = usePathname();
  const norm = pathname && pathname.length > 1 && pathname.endsWith("/") ? pathname.slice(0, -1) : pathname;
  const isLogin = norm === "/login";

  return (
    <I18nProvider>
      <div className="admin-theme min-h-screen">
        <AuthProvider>
          {isLogin ? (
            children
          ) : (
            <AuthGate>
              <AdminShell>{children}</AdminShell>
            </AuthGate>
          )}
        </AuthProvider>
      </div>
    </I18nProvider>
  );
}
