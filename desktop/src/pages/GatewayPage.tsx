import {
  Alert,
  Button,
  Card,
  Descriptions,
  Form,
  Input,
  InputNumber,
  Popconfirm,
  Select,
  Space,
  Switch,
  Tag,
  Typography,
  message,
} from "antd";
import { useEffect, useState } from "react";
import { api } from "../api";
import type {
  GatewaySettingsView,
  GatewayStatus,
  ProcessPhase,
} from "../types";

const phaseText: Record<ProcessPhase, string> = {
  running: "运行中",
  stopped: "已停止",
  starting: "启动中",
  error: "需要处理",
  unavailable: "未安装",
};

export default function GatewayPage({
  phaseColor,
}: {
  phaseColor: Record<ProcessPhase, string>;
}) {
  const [status, setStatus] = useState<GatewayStatus | null>(null);
  const [settings, setSettings] = useState<GatewaySettingsView | null>(null);
  const [form] = Form.useForm();
  const [messageApi, contextHolder] = message.useMessage();

  useEffect(() => {
    const refresh = () =>
      api.gatewayStatus().then(setStatus).catch(() => setStatus(null));
    refresh();
    const timer = window.setInterval(refresh, 2000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    api.gatewaySettings().then((view) => {
      setSettings(view);
      form.setFieldsValue(view.values);
    });
  }, [form]);

  if (!status) {
    return <Card loading />;
  }

  const runCommand = async (command: string, success: string) => {
    try {
      setStatus(await api.gatewayCommand(command));
      messageApi.success(success);
    } catch (error) {
      messageApi.error(String(error));
    }
  };

  return (
    <Space direction="vertical" size={20} className="full-width">
      {contextHolder}
      <Typography.Title level={3} className="page-title">
        媒体网关
      </Typography.Title>
      <Card
        title="进程状态"
        extra={
          <Tag color={phaseColor[status.phase]} bordered={false}>
            {phaseText[status.phase]}
          </Tag>
        }
        actions={[
          <Button key="start" type="primary" onClick={() => void runCommand("start_gateway", "网关已启动")}>
            启动
          </Button>,
          <Button key="restart" onClick={() => void runCommand("restart_gateway", "网关已重启")}>
            重启
          </Button>,
          <Popconfirm
            key="stop"
            title="确定停止媒体网关吗？"
            onConfirm={() => void runCommand("stop_gateway", "网关已停止")}
          >
            <Button danger>停止</Button>
          </Popconfirm>,
        ]}
      >
        <Alert type={status.phase === "running" ? "success" : "info"} message={status.message} showIcon />
        {!status.urlMatchesService && (
          <Alert
            type="warning"
            showIcon
            message={`网关地址 ${status.url} 与后端 ${status.serviceUrl || "（空）"} 不一致`}
          />
        )}
        <Descriptions className="page-descriptions" column={3} size="small" bordered>
          <Descriptions.Item label="PID">{status.pid ?? "—"}</Descriptions.Item>
          <Descriptions.Item label="自动恢复">
            {status.autoRestart ? "开启" : "关闭"}
          </Descriptions.Item>
          <Descriptions.Item label="健康检查">
            {status.health ? (status.health.ok ? "正常" : "异常") : "—"}
          </Descriptions.Item>
          <Descriptions.Item label="出图" span={1}>
            {status.health?.diffusion ?? "—"}
          </Descriptions.Item>
          <Descriptions.Item label="语音">
            {status.health?.tts ?? "—"}
          </Descriptions.Item>
          <Descriptions.Item label="识别">
            {status.health?.asrEngine ?? "—"}
          </Descriptions.Item>
        </Descriptions>
      </Card>
      <Card
        title="媒体网关配置"
        extra={settings ? <Tag bordered={false}>{settings.envFile}</Tag> : null}
      >
        <Typography.Paragraph type="secondary">
          配置写入独立的 media-gateway.env；保存后自动重启网关。
        </Typography.Paragraph>
        <Form
          form={form}
          layout="vertical"
          onFinish={async (values) => {
            const result = await api.updateGatewaySettings(values);
            setSettings(result.view);
            form.setFieldsValue(result.view.values);
            messageApi.success(result.warning ?? (result.restarted ? "已保存并重启网关" : "已保存"));
            setStatus(await api.gatewayStatus());
          }}
        >
          {(settings?.schema ?? []).map((item) => (
            <Form.Item key={item.key} name={item.key} label={item.description}>
              {item.kind === "boolean" ? (
                <Switch />
              ) : item.kind === "choice" ? (
                <Select
                  options={item.choices.map((choice) => ({ value: choice, label: choice }))}
                />
              ) : item.kind === "integer" ? (
                <InputNumber className="full-width" />
              ) : (
                <Input />
              )}
            </Form.Item>
          ))}
          <Button type="primary" htmlType="submit">
            保存并应用
          </Button>
        </Form>
      </Card>
    </Space>
  );
}
