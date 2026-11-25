import theme from "antd/es/theme";
import { useEffect, useState } from "react";

/** 用户可选的外观模式；system 跟随操作系统。 */
export type ThemeMode = "light" | "dark" | "system";

const STORAGE_KEY = "zhiwei-theme-mode";

function systemPrefersDark(): boolean {
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

/** 当前实际生效的明暗（system 解析后的结果）。 */
export function useResolvedDark(mode: ThemeMode): boolean {
  const [systemDark, setSystemDark] = useState(systemPrefersDark);

  useEffect(() => {
    if (mode !== "system") return;
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = (event: MediaQueryListEvent) => setSystemDark(event.matches);
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, [mode]);

  return mode === "system" ? systemDark : mode === "dark";
}

export function loadThemeMode(): ThemeMode {
  const stored = localStorage.getItem(STORAGE_KEY);
  return stored === "light" || stored === "dark" || stored === "system"
    ? stored
    : "system";
}

export function saveThemeMode(mode: ThemeMode) {
  localStorage.setItem(STORAGE_KEY, mode);
}

/** 中性配色：明暗两套共享的语义色，不含品牌绿。 */
const neutral = {
  borderRadius: 18,
  fontSize: 14,
};

export function buildTheme(dark: boolean) {
  return {
    algorithm: dark ? theme.darkAlgorithm : theme.defaultAlgorithm,
    token: neutral,
  };
}
