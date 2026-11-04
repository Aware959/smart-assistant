import {
  AppstoreOutlined,
  FileTextOutlined,
  SendOutlined,
  AudioOutlined,
  UserOutlined,
} from "@ant-design/icons";
import { ConfigProvider, Layout, Menu, theme } from "antd";
import { useEffect, useState } from "react";
import FilesPage from "./pages/FilesPage";
import GatewayPage from "./pages/GatewayPage";
import IdentityPage from "./pages/IdentityPage";
import OverviewPage from "./pages/OverviewPage";
import ServicePage from "./pages/ServicePage";
import type { ProcessPhase } from "./types";

const phaseColor: Record<ProcessPhase, string> = {
  running: "#52c41a",
  stopped: "#8c8c8c",
  starting: "#fadb14",
  error: "#ff4d4f",
  unavailable: "#595959",
};

const items = [
  { key: "overview", icon: <AppstoreOutlined />, label: "概览" },
  { key: "service", icon: <SendOutlined />, label: "通道服务" },
  { key: "identity", icon: <UserOutlined />, label: "身份演化" },
  { key: "gateway", icon: <AudioOutlined />, label: "媒体网关" },
  { key: "files", icon: <FileTextOutlined />, label: "本机文件" },
];

function currentPage(): string {
  const hash = window.location.hash.replace(/^#\/?/, "");
  return items.some((item) => item.key === hash) ? hash : "overview";
}

export default function App() {
  const [page, setPage] = useState(currentPage);
  const { darkAlgorithm } = theme;

  useEffect(() => {
    const handleHashChange = () => setPage(currentPage());
    window.addEventListener("hashchange", handleHashChange);
    return () => window.removeEventListener("hashchange", handleHashChange);
  }, []);

  return (
    <ConfigProvider
      theme={{
        algorithm: darkAlgorithm,
        token: {
          colorPrimary: "#52c41a",
          colorInfo: "#52c41a",
          borderRadius: 10,
          colorBgBase: "#0f1e19",
          colorBgContainer: "#162c24",
          colorBgElevated: "#1b332b",
          colorBorder: "#2c4d41",
          colorText: "#f2f7f4",
          colorTextSecondary: "#b7c8bf",
        },
      }}
    >
      <Layout className="app-shell">
        <Layout.Sider width={224} className="app-sider" theme="dark">
          <div className="brand">
            <div className="brand-orbit" />
            <div>
              <div className="brand-eyebrow">见微知著</div>
              <div className="brand-name">知微</div>
            </div>
          </div>
          <Menu
            mode="inline"
            items={items}
            selectedKeys={[page]}
            onClick={({ key }) => {
              window.location.hash = `#/${key}`;
            }}
          />
          <div className="sider-foot">关闭窗口仅隐藏到托盘</div>
        </Layout.Sider>
        <Layout.Content className="app-content">
          {page === "overview" && <OverviewPage phaseColor={phaseColor} />}
          {page === "service" && <ServicePage />}
          {page === "identity" && <IdentityPage />}
          {page === "gateway" && <GatewayPage phaseColor={phaseColor} />}
          {page === "files" && <FilesPage />}
        </Layout.Content>
      </Layout>
    </ConfigProvider>
  );
}
