"use client";
// New-design app shell (design doc §4, 14A).
//
//   < 640      top bar (back · title · search) + floating 4-tab bar (56, 8 + safe area)
//   640–1023   72px icon rail with the four anchors (icon + 12px label)
//   ≥ 1024     240px sidebar: identity, search, anchors with their groups
//
// Every size: skip link, <nav> + <main>, focus moves to the new page's
// heading on route change and its title is announced. ⌘K / Ctrl+K opens
// search anywhere.
import Link from "next/link";
import { usePathname } from "next/navigation";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { CaretLeft, ChartLine, GearSix, House, MagnifyingGlass, SignOut, SquaresFour, type Icon } from "@phosphor-icons/react";
import { useAuth } from "@/lib/hooks/useAuth";
import { DEFAULT_DEVICE_LABEL, useDeviceLabel } from "@/lib/publicStatus";
import { getApiBase } from "@/lib/api/client";
import { anchorFor, GROUPS, HUBS, normPath, routeFor, ROUTES, type Anchor, type RouteGroup } from "@/lib/routes";
import { CommandPalette } from "./CommandPalette";
import { StatusIcons } from "./StatusIcons";
import { LangSwitch } from "./LangSwitch";
import { AlertBanner } from "./AlertBanner";
import { LoginDialog } from "./LoginDialog";

const ANCHORS: { id: Anchor; href: string; tKey: string; fallback: string; icon: Icon }[] = [
  { id: "home", href: "/", tKey: "anchor.home", fallback: "Home", icon: House },
  { id: "charts", href: HUBS.charts.href, tKey: HUBS.charts.tKey, fallback: HUBS.charts.fallback, icon: ChartLine },
  { id: "functions", href: HUBS.functions.href, tKey: HUBS.functions.tKey, fallback: HUBS.functions.fallback, icon: SquaresFour },
  { id: "system", href: HUBS.system.href, tKey: HUBS.system.tKey, fallback: HUBS.system.fallback, icon: GearSix },
];


export function Shell({ children }: { children: ReactNode }) {
  const { t } = useTranslation();
  const pathname = normPath(usePathname() ?? "/");
  const { logout } = useAuth();
  const deviceLabel = useDeviceLabel();
  // The exported <title> is fixed at build time; follow the real device name.
  useEffect(() => {
    if (deviceLabel !== DEFAULT_DEVICE_LABEL) document.title = `${deviceLabel.split(" · ")[0]} Admin`;
  }, [deviceLabel]);
  const [search, setSearch] = useState(false);
  const [apiHost, setApiHost] = useState("");
  const [announce, setAnnounce] = useState("");
  const mainRef = useRef<HTMLElement>(null);
  const lastPath = useRef<string | null>(null);

  const anchor = anchorFor(pathname);
  const route = routeFor(pathname);
  const hub = ANCHORS.find((a) => a.id === anchor)!;
  const isTop = pathname === "/" || Object.values(HUBS).some((h) => h.href === pathname);
  const pageTitle = isTop ? t(hub.tKey, hub.fallback) : route ? t(route.tKey, route.fallback) : "";

  useEffect(() => {
    try {
      setApiHost(new URL(getApiBase()).host);
    } catch {
      /* malformed base: leave empty */
    }
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setSearch((s) => !s);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Route change: announce the title and move focus to the page heading.
  useEffect(() => {
    // Skip the first render (and StrictMode's second effect run in dev):
    // only a real route change moves focus.
    if (lastPath.current === null || lastPath.current === pathname) {
      lastPath.current = pathname;
      return;
    }
    lastPath.current = pathname;
    setAnnounce(pageTitle);
    const h = mainRef.current?.querySelector<HTMLElement>("h1");
    if (h) {
      h.tabIndex = -1;
      h.focus({ preventScroll: false });
    } else {
      mainRef.current?.focus();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pathname]);

  // Colour family: each route group has its own block colour (nd.css).
  const family = pathname === "/" ? "home" : (routeFor(pathname)?.group ?? anchor);

  return (
    <div className="nd min-h-dvh bg-nd-bg text-nd-t1 sm:flex" data-family={family}>
      <a href="#main" className="nd-skip">
        {t("nd.skipToMain", "Skip to main content")}
      </a>

      {/* ≥1024 sidebar */}
      <aside className="nd-sidebar hidden lg:flex">
        <Link href="/" className="nd-identity">
          {deviceLabel}
        </Link>
        <button type="button" className="nd-searchbtn" onClick={() => setSearch(true)}>
          <MagnifyingGlass size={18} weight="bold" aria-hidden />
          <span className="flex-1 text-start">{t("nd.search", "Search")}</span>
          <kbd className="nd-kbd">⌘K</kbd>
        </button>
        <nav aria-label={t("nd.mainNav", "Main")} className="nd-sidebar__nav">
          <SideLink href="/" current={pathname === "/"} icon={House} label={t("anchor.home", "Home")} />
          {(["charts", "functions", "system"] as const).map((a) => (
            <SideSection key={a} anchor={a} pathname={pathname} />
          ))}
        </nav>
      </aside>

      {/* 640–1023 rail */}
      <nav aria-label={t("nd.mainNav", "Main")} className="nd-rail hidden sm:flex lg:hidden">
        <button type="button" className="nd-rail__item" onClick={() => setSearch(true)} aria-label={t("nd.search", "Search")}>
          <MagnifyingGlass size={22} weight="bold" aria-hidden />
          <span>{t("nd.searchShort", "Search")}</span>
        </button>
        {ANCHORS.map((a) => (
          <Link key={a.id} href={a.href} className="nd-rail__item" aria-current={anchor === a.id ? "page" : undefined}>
            <a.icon size={22} weight={anchor === a.id ? "fill" : "bold"} aria-hidden />
            <span>{t(a.tKey, a.fallback)}</span>
          </Link>
        ))}
      </nav>

      <div className="flex min-w-0 flex-1 flex-col">
        <header className="nd-topbar">
          {!isTop && (
            <Link href={hub.href} className="nd-back sm:hidden" aria-label={t("nd.backTo", "Back to {{page}}", { page: t(hub.tKey, hub.fallback) })}>
              <CaretLeft size={22} weight="bold" aria-hidden />
            </Link>
          )}
          <div className="ms-auto flex items-center gap-1">
            <StatusIcons />
            <button type="button" className="nd-iconbtn lg:hidden" onClick={() => setSearch(true)} aria-label={t("nd.search", "Search")}>
              <MagnifyingGlass size={20} weight="bold" aria-hidden />
            </button>
            <span className="hidden md:inline-flex">
              <LangSwitch />
            </span>
            {apiHost && <span className="nd-mono hidden px-2 text-[13px] text-nd-t3 xl:inline">{apiHost}</span>}
            <button type="button" className="nd-iconbtn" onClick={logout} aria-label={t("common.logOut")} title={t("common.logOut")}>
              <SignOut size={20} weight="bold" aria-hidden />
            </button>
          </div>
        </header>

        <main id="main" ref={mainRef} tabIndex={-1} className="nd-main">
          <AlertBanner />
          {children}
        </main>
      </div>

      {/* < 640 floating tab bar */}
      <nav aria-label={t("nd.mainNav", "Main")} className="nd-tabbar sm:hidden">
        {ANCHORS.map((a) => (
          <Link key={a.id} href={a.href} className="nd-tabbar__item" aria-current={anchor === a.id ? "page" : undefined}>
            <a.icon size={22} weight={anchor === a.id ? "fill" : "bold"} aria-hidden />
            <span>{t(a.tKey, a.fallback)}</span>
          </Link>
        ))}
      </nav>

      <CommandPalette open={search} onOpenChange={setSearch} />
      <LoginDialog />
      <div className="sr-only" aria-live="polite" aria-atomic="true">
        {announce}
      </div>
    </div>
  );
}

function SideLink({ href, current, icon: I, label }: { href: string; current: boolean; icon: Icon; label: string }) {
  return (
    <Link href={href} className="nd-side__link" aria-current={current ? "page" : undefined}>
      <I size={20} weight={current ? "fill" : "bold"} aria-hidden />
      <span className="min-w-0 truncate">{label}</span>
    </Link>
  );
}

function SideSection({ anchor, pathname }: { anchor: Exclude<Anchor, "home">; pathname: string }) {
  const { t } = useTranslation();
  const hub = HUBS[anchor];
  const groups = (Object.keys(GROUPS) as RouteGroup[]).filter((g) => GROUPS[g].anchor === anchor);
  const cur = routeFor(pathname)?.href;
  return (
    <div className="mt-5">
      <Link href={hub.href} className="nd-side__anchor" aria-current={pathname === hub.href ? "page" : undefined}>
        {t(hub.tKey, hub.fallback)}
      </Link>
      {groups.map((g) => {
        const items = ROUTES.filter((r) => r.group === g && !r.hidden);
        return (
          <div key={g}>
            {groups.length > 1 && <div className="nd-side__group">{t(GROUPS[g].tKey, GROUPS[g].fallback)}</div>}
            {items.map((r) => (
              <SideLink key={r.href} href={r.href} current={cur === r.href} icon={r.icon} label={t(r.tKey, r.fallback)} />
            ))}
          </div>
        );
      })}
    </div>
  );
}
