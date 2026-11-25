import { Segmented } from "antd";
import {
  MonitorOutlined,
  MoonOutlined,
  SunOutlined,
} from "./icons";
import { ConfigProvider, Layout } from "antd";
import { useEffect, useState } from "react";
import IdentityPage from "./pages/IdentityPage";
import SystemPage from "./pages/SystemPage";
import {
  buildTheme,
  loadThemeMode,
  saveThemeMode,
  useResolvedDark,
  type ThemeMode,
} from "./theme";

const tabs = [
  { key: "identity", label: "首页" },
  { key: "system", label: "系统" },
];

const modeOptions = [
  { value: "light", icon: <SunOutlined /> },
  { value: "system", icon: <MonitorOutlined /> },
  { value: "dark", icon: <MoonOutlined /> },
];

function currentTab(): string {
  const hash = window.location.hash.replace(/^#\/?/, "");
  return tabs.some((tab) => tab.key === hash) ? hash : "identity";
}

function TabBar({
  page,
  onPick,
}: {
  page: string;
  onPick: (key: string) => void;
}) {
  return (
    <nav className="tab-bar">
      {tabs.map((tab) => (
        <button
          key={tab.key}
          type="button"
          className={`tab-item${page === tab.key ? " active" : ""}`}
          onClick={() => onPick(tab.key)}
        >
          {tab.label}
        </button>
      ))}
    </nav>
  );
}

export default function App() {
  const [page, setPage] = useState(currentTab);
  const [mode, setMode] = useState<ThemeMode>(loadThemeMode);
  const dark = useResolvedDark(mode);

  useEffect(() => {
    const handleHashChange = () => setPage(currentTab());
    window.addEventListener("hashchange", handleHashChange);
    return () => window.removeEventListener("hashchange", handleHashChange);
  }, []);

  const pick = (key: string) => {
    window.location.hash = `#/${key}`;
    setPage(key);
  };

  return (
    <ConfigProvider theme={buildTheme(dark)}>
      <Layout className={`app-shell${dark ? " dark" : " light"}`}>
        <header className="top-bar">
          <div className="brand">
            <div className="brand-name">知微</div>
            <div className="brand-eyebrow">见微知著</div>
          </div>
          <Segmented
            className="theme-switch"
            value={mode}
            options={modeOptions}
            onChange={(value) => {
              setMode(value as ThemeMode);
              saveThemeMode(value as ThemeMode);
            }}
          />
        </header>
        <Layout.Content className="app-content">
          {page === "identity" && <IdentityPage />}
          {page === "system" && <SystemPage />}
        </Layout.Content>
        <TabBar page={page} onPick={pick} />
      </Layout>
    </ConfigProvider>
  );
}
