// Light / dark / system theme. The choice lives in this browser only
// (localStorage "u60.theme"); "system" follows prefers-color-scheme live.
// The same logic runs inline in app/layout.tsx before first paint
// (THEME_INIT_SCRIPT), so the page never flashes the wrong theme.

export type ThemeChoice = "system" | "light" | "dark";

export const THEME_KEY = "u60.theme";

export function resolveTheme(choice: ThemeChoice | null, systemDark: boolean): "light" | "dark" {
  if (choice === "light" || choice === "dark") return choice;
  return systemDark ? "dark" : "light";
}

export function readThemeChoice(): ThemeChoice {
  if (typeof window === "undefined") return "system";
  const v = window.localStorage.getItem(THEME_KEY);
  return v === "light" || v === "dark" ? v : "system";
}

export function applyTheme(choice: ThemeChoice) {
  if (typeof window === "undefined") return;
  if (choice === "system") window.localStorage.removeItem(THEME_KEY);
  else window.localStorage.setItem(THEME_KEY, choice);
  const dark = window.matchMedia("(prefers-color-scheme: dark)").matches;
  document.documentElement.dataset.theme = resolveTheme(choice, dark);
}

// Inline, dependency-free copy for <head>. Also keeps "system" in sync
// when the OS switches while the page is open.
export const THEME_INIT_SCRIPT = `(function(){try{
var k=${JSON.stringify(THEME_KEY)},m=window.matchMedia("(prefers-color-scheme: dark)");
function set(){var c=localStorage.getItem(k);document.documentElement.dataset.theme=
(c==="light"||c==="dark")?c:(m.matches?"dark":"light");}
set();m.addEventListener("change",function(){var c=localStorage.getItem(k);if(c!=="light"&&c!=="dark")set();});
}catch(e){document.documentElement.dataset.theme="light";}})();`;
