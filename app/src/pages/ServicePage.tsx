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
import { useEffect, useMemo, useState } from "react";
import { api } from "../api";
import type { ServiceStatus, SettingsSchemaItem, SettingsView } from "../types";

const phaseText: Record<ServiceStatus["phase"], string> = {
  running: "运行中",
  stopped: "已停止",
  starting: "启动中",
  error: "需要处理",
  unavailable: "未安装",
};

/** 密钥占位符：服务端对未改动的密钥回传这个值，提交时原样带回即视为"不改"。 */
const SECRET_MASK = "********";

/** 按分组聚合 schema，保留服务端给定的组序。 */
function groupByGroup(items: SettingsSchemaItem[]) {
  const groups: { key: string; label: string; items: SettingsSchemaItem[] }[] = [];
  for (const item of items) {
    const last = groups[groups.length - 1];
    if (last && last.key === item.group) {
      last.items.push(item);
      continue;
    }
    groups.push({ key: item.group, label: item.group_label, items: [item] });
  }
  return groups;
}

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

  const groups = useMemo(() => groupByGroup(schema), [schema]);
  const editableGroups = groups.filter((group) =>
    group.items.some((item) => item.editable),
  );
  const readOnlyGroups = groups.filter(
    (group) => !group.items.some((item) => item.editable),
  );

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

  const save = async (values: Record<string, string | number | boolean>) => {
    if (!view) return;
    // 只提交真正改过的字段：面板展示的缺省值不该被当成一次修改写回。
    const changes: Record<string, string | number | boolean> = {};
    for (const [key, value] of Object.entries(values)) {
      const item = schema.find((candidate) => candidate.key === key);
      if (!item) continue;
      const before = view.values[key];
      if (JSON.stringify(before) === JSON.stringify(value)) continue;
      changes[key] = value;
    }
    if (Object.keys(changes).length === 0) {
      messageApi.info("没有改动");
      return;
    }
    const result = await api.updateSettings(view.revision, changes);
    if (result.ok && result.data) {
      setView(result.data);
      form.setFieldsValue(result.data.values);
      messageApi.success(`已保存，revision ${result.data.revision}`);
    } else {
      messageApi.error(result.error ?? "保存失败");
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
        extra={
          <Tag color={service.phase === "running" ? "green" : "default"}>
            {phaseText[service.phase]}
          </Tag>
        }
        actions={[
          <Button
            key="start"
            type="primary"
            onClick={() => void runCommand("start_service", "服务已启动")}
          >
            启动
          </Button>,
          <Button
            key="restart"
            onClick={() => void runCommand("restart_service", "服务已重启")}
          >
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
        <Alert
          type={service.phase === "running" ? "success" : "info"}
          message={service.message}
          showIcon
        />
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

      <Form form={form} layout="vertical" onFinish={(values) => void save(values)}>
        <Space direction="vertical" size={20} className="full-width">
          {editableGroups.map((group) => (
            <Card
              key={group.key}
              title={group.label}
              extra={
                view ? (
                  <Tag bordered={false}>revision {view.revision}</Tag>
                ) : null
              }
            >
              <Typography.Paragraph type="secondary">
                改动即时生效；标注"需重启"的项要重启通道服务才生效。
              </Typography.Paragraph>
              {group.items
                .filter((item) => item.editable)
                .map((item) => (
                  <Form.Item
                    key={item.key}
                    name={item.key}
                    label={
                      <Space size={8}>
                        <span>{item.description}</span>
                        {item.restart_required ? (
                          <Tag color="orange" bordered={false}>
                            需重启
                          </Tag>
                        ) : null}
                      </Space>
                    }
                    extra={
                      item.kind === "secret" ? (
                        <Typography.Text type="secondary">
                          留空或保持 ******** 表示不改；密钥不会回显。
                        </Typography.Text>
                      ) : null
                    }
                  >
                    {item.kind === "boolean" ? (
                      <Switch />
                    ) : item.kind === "secret" ? (
                      <Input.Password placeholder={SECRET_MASK} />
                    ) : item.kind === "string" ? (
                      <Input />
                    ) : (
                      <InputNumber className="full-width" />
                    )}
                  </Form.Item>
                ))}
            </Card>
          ))}
          <Button type="primary" htmlType="submit">
            保存运行配置
          </Button>
        </Space>
      </Form>

      {readOnlyGroups.length > 0 ? (
        <Card title="只读配置">
          <Typography.Paragraph type="secondary">
            这些项由 .env 决定，改它要改配置文件并重启，不在面板里编辑。
          </Typography.Paragraph>
          {readOnlyGroups.map((group) => (
            <div key={group.key} className="meta-grid">
              {group.items.map((item) => (
                <div key={item.key}>
                  <Typography.Text type="secondary">
                    {item.description}
                  </Typography.Text>
                  <div className="mono">
                    {item.kind === "secret"
                      ? item.current
                        ? "已配置"
                        : "未配置"
                      : String(item.current ?? "—")}
                  </div>
                </div>
              ))}
            </div>
          ))}
        </Card>
      ) : null}
    </Space>
  );
}
