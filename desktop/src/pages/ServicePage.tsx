import {
  Alert,
  Button,
  Card,
  Form,
  Input,
  InputNumber,
  Popconfirm,
  Space,
  Switch,
  Tag,
  Typography,
  message,
} from "antd";
import { useEffect, useState } from "react";
import { api } from "../api";
import type { ServiceStatus, SettingsSchemaItem, SettingsView } from "../types";

const phaseText: Record<ServiceStatus["phase"], string> = {
  running: "运行中",
  stopped: "已停止",
  starting: "启动中",
  error: "需要处理",
  unavailable: "未安装",
};

export default function ServicePage() {
  const [service, setService] = useState<ServiceStatus | null>(null);
  const [view, setView] = useState<SettingsView | null>(null);
  const [schema, setSchema] = useState<SettingsSchemaItem[]>([]);
  const [form] = Form.useForm();
  const [messageApi, contextHolder] = message.useMessage();

  useEffect(() => {
    const refresh = () =>
      api.serviceStatus().then(setService).catch(() => setService(null));
    refresh();
    const timer = window.setInterval(refresh, 2000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    Promise.all([api.settingsView(), api.settingsSchema()]).then(([viewResult, schemaResult]) => {
      if (viewResult.ok && viewResult.data) {
        setView(viewResult.data);
        form.setFieldsValue(viewResult.data.values);
      }
      if (schemaResult.ok && schemaResult.data) {
        setSchema(schemaResult.data);
      }
    });
  }, [form]);

  if (!service) {
    return <Card loading />;
  }

  const runCommand = async (command: string, success: string) => {
    try {
      setService(await api.serviceCommand(command));
      messageApi.success(success);
    } catch (error) {
      messageApi.error(String(error));
    }
  };

  return (
    <Space direction="vertical" size={20} className="full-width">
      {contextHolder}
      <Typography.Title level={3} className="page-title">
        通道服务
      </Typography.Title>
      <Card
        title="Telegram 守护"
        extra={<Tag color={service.phase === "running" ? "green" : "default"}>{phaseText[service.phase]}</Tag>}
        actions={[
          <Button key="start" type="primary" onClick={() => void runCommand("start_service", "服务已启动")}>
            启动
          </Button>,
          <Button key="restart" onClick={() => void runCommand("restart_service", "服务已重启")}>
            重启
          </Button>,
          <Popconfirm
            key="stop"
            title="确定停止通道服务吗？"
            onConfirm={() => void runCommand("stop_service", "服务已停止")}
          >
            <Button danger>停止</Button>
          </Popconfirm>,
        ]}
      >
        <Alert type={service.phase === "running" ? "success" : "info"} message={service.message} showIcon />
        <div className="meta-grid">
          <div>
            <Typography.Text type="secondary">进程 PID</Typography.Text>
            <div className="mono">{service.pid ?? "—"}</div>
          </div>
          <div>
            <Typography.Text type="secondary">重试次数</Typography.Text>
            <div className="mono">{service.restartAttempts}</div>
          </div>
          <div>
            <Typography.Text type="secondary">自动恢复</Typography.Text>
            <div>{service.autoRestart ? "开启" : "关闭"}</div>
          </div>
        </div>
      </Card>
      <Card
        title="运行时配置"
        extra={view ? <Tag bordered={false}>revision {view.revision}</Tag> : null}
      >
        <Typography.Paragraph type="secondary">
          配置来自 zhiwei-service 数据库，保存后即时生效。
        </Typography.Paragraph>
        <Form
          form={form}
          layout="vertical"
          onFinish={async (values) => {
            if (!view) return;
            const result = await api.updateSettings(view.revision, values);
            if (result.ok && result.data) {
              setView(result.data);
              form.setFieldsValue(result.data.values);
              messageApi.success(`已保存，revision ${result.data.revision}`);
            } else {
              messageApi.error(result.error ?? "保存失败");
            }
          }}
        >
          {schema.map((item) => (
            <Form.Item key={item.key} name={item.key} label={item.description}>
              {item.kind === "boolean" ? (
                <Switch />
              ) : item.kind === "string" ? (
                <Input />
              ) : (
                <InputNumber className="full-width" />
              )}
            </Form.Item>
          ))}
          <Button type="primary" htmlType="submit">
            保存运行配置
          </Button>
        </Form>
      </Card>
    </Space>
  );
}
