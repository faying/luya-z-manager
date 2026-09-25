"use client";

import { usePathname } from "next/navigation";
import { AuthProvider } from "@/lib/hooks/useAuth";
import { AuthGate } from "@/components/nd/shell/AuthGate";
import { I18nProvider } from "@/components/nd/shell/I18nProvider";
import { Shell } from "@/components/nd/shell/Shell";
import { ToastProvider } from "@/components/nd";

export default function AdminLayout({ children }: { children: React.ReactNode }) {
  const pathname = usePathname();
  const norm = pathname && pathname.length > 1 && pathname.endsWith("/") ? pathname.slice(0, -1) : pathname;
  const isLogin = norm === "/login";

  return (
    <I18nProvider>
      <ToastProvider>
        <AuthProvider>
          {isLogin ? (
            children
          ) : (
            <AuthGate>
              <Shell>{children}</Shell>
            </AuthGate>
          )}
        </AuthProvider>
      </ToastProvider>
    </I18nProvider>
  );
}
