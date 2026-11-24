import {
  Card,
  Col,
  Empty,
  Row,
  Space,
  Spin,
  Statistic,
  Tag,
  Typography,
} from "antd";
import { useEffect, useState } from "react";
import { api } from "../api";
import type { ProcessPhase, ServiceStatus } from "../types";

const phaseText: Record<ProcessPhase, string> = {
  running: "运行中",
  stopped: "已停止",
  starting: "启动中",
  error: "需要处理",
  unavailable: "未安装",
};

export default function OverviewPage({
  phaseColor,
}: {
  phaseColor: Record<ProcessPhase, string>;
}) {
  const [service, setService] = useState<ServiceStatus | null>(null);

  useEffect(() => {
    const refresh = () => {
      api.serviceStatus().then(setService).catch(() => setService(null));
    };
    refresh();
    const timer = window.setInterval(refresh, 2000);
    return () => window.clearInterval(timer);
  }, []);

  if (!service) {
    return (
      <div className="center-loading">
        <Spin size="large" />
      </div>
    );
  }

  return (
    <Space direction="vertical" size={20} className="full-width">
      <Typography.Title level={3} className="page-title">
        概览
      </Typography.Title>
      <Row gutter={[20, 20]}>
        <Col xs={24} xl={12}>
          <Card
            title="通道守护"
            extra={
              <Tag color={phaseColor[service.phase]} bordered={false}>
                {phaseText[service.phase]}
              </Tag>
            }
            loading={false}
          >
            <Typography.Paragraph type="secondary">
              {service.message}
            </Typography.Paragraph>
            <Row gutter={16}>
              <Col span={12}>
                <Statistic title="进程 PID" value={service.pid ?? "—"} />
              </Col>
              <Col span={12}>
                <Statistic
                  title="自动恢复"
                  value={service.autoRestart ? "开启" : "关闭"}
                />
              </Col>
            </Row>
          </Card>
        </Col>
      </Row>
      <Card>
        <Typography.Paragraph type="secondary">
          通道守护负责 Telegram 长轮询与本机 MCP 服务器（MCP 工具按需拉起，不常驻）。
        </Typography.Paragraph>
        <Empty description={false} image={Empty.PRESENTED_IMAGE_SIMPLE} />
      </Card>
    </Space>
  );
}
