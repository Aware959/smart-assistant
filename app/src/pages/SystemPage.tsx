import {
  Button,
  Card,
  Input,
  InputNumber,
  Space,
  Switch,
  Tag,
  Typography,
  message,
} from "antd";
import { useEffect, useMemo, useState } from "react";
import { api } from "../api";
import type { SettingsSchemaItem, SettingsView } from "../types";

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

export default function SystemPage() {
  const [view, setView] = useState<SettingsView | null>(null);
  const [schema, setSchema] = useState<SettingsSchemaItem[]>([]);
  const [draft, setDraft] = useState<Record<string, string | number | boolean>>({});
  const [saving, setSaving] = useState(false);
  const [messageApi, contextHolder] = message.useMessage();

  useEffect(() => {
    Promise.all([api.settingsView(), api.settingsSchema()]).then(
      ([viewResult, schemaResult]) => {
        if (viewResult.ok && viewResult.data) {
          setView(viewResult.data);
          setDraft(viewResult.data.values);
        }
        if (schemaResult.ok && schemaResult.data) {
          setSchema(schemaResult.data);
        }
      },
    );
  }, []);

  const groups = useMemo(() => groupByGroup(schema), [schema]);
  const editableGroups = groups.filter((group) =>
    group.items.some((item) => item.editable),
  );
  const readOnlyGroups = groups.filter(
    (group) => !group.items.some((item) => item.editable),
  );

  const rowValue = (item: SettingsSchemaItem) => draft[item.key];
  const rowChanged = (item: SettingsSchemaItem) => {
    const before = view?.values[item.key];
    const after = draft[item.key];
    return JSON.stringify(before) !== JSON.stringify(after);
  };

  const setRow = (item: SettingsSchemaItem, value: string | number | boolean) => {
    setDraft((prev) => ({ ...prev, [item.key]: value }));
  };

  const save = async () => {
    if (!view) return;
    // 只提交真正改过的字段：面板展示的缺省值不该被当成一次修改写回。
    const changes: Record<string, string | number | boolean> = {};
    for (const item of schema) {
      if (!item.editable) continue;
      if (!rowChanged(item)) continue;
      const value = draft[item.key];
      if (value === undefined) continue;
      changes[item.key] = value;
    }
    if (Object.keys(changes).length === 0) {
      messageApi.info("没有改动");
      return;
    }
    setSaving(true);
    const result = await api.updateSettings(view.revision, changes);
    setSaving(false);
    if (result.ok && result.data) {
      setView(result.data);
      setDraft(result.data.values);
      messageApi.success(`已保存，revision ${result.data.revision}`);
    } else {
      messageApi.error(result.error ?? "保存失败");
    }
  };

  const dirtyCount = schema.filter((item) => item.editable && rowChanged(item)).length;

  const renderControl = (item: SettingsSchemaItem) => {
    const value = rowValue(item);
    if (item.kind === "boolean") {
      return <Switch checked={value === true} onChange={(v) => setRow(item, v)} />;
    }
    if (item.kind === "secret") {
      return (
        <Input.Password
          className="row-input"
          placeholder={SECRET_MASK}
          value={typeof value === "string" ? value : ""}
          onChange={(e) => setRow(item, e.target.value)}
        />
      );
    }
    if (item.kind === "string") {
      return (
        <Input
          className="row-input"
          value={typeof value === "string" ? value : ""}
          onChange={(e) => setRow(item, e.target.value)}
        />
      );
    }
    return (
      <InputNumber
        className="row-input"
        value={typeof value === "number" ? value : undefined}
        onChange={(v) => setRow(item, v ?? 0)}
      />
    );
  };

  return (
    <Space direction="vertical" size={20} className="full-width">
      {contextHolder}
      <Typography.Title level={2} className="page-title">
        系统
      </Typography.Title>

      {editableGroups.map((group) => (
        <Card key={group.key} title={group.label} className="setting-group">
          {group.items
            .filter((item) => item.editable)
            .map((item) => (
              <div key={item.key} className="setting-row">
                <div className="row-label">
                  <span className="row-title">{item.description}</span>
                  {item.restart_required ? (
                    <Tag color="orange" bordered={false}>
                      需重启
                    </Tag>
                  ) : null}
                  {rowChanged(item) ? <Tag bordered={false}>已改</Tag> : null}
                </div>
                <div className="row-control">{renderControl(item)}</div>
              </div>
            ))}
        </Card>
      ))}

      <div className="save-dock">
        <Space>
          {dirtyCount > 0 ? (
            <Button onClick={() => view && setDraft(view.values)}>还原</Button>
          ) : null}
          <Button type="primary" loading={saving} onClick={() => void save()}>
            保存{dirtyCount > 0 ? `（${dirtyCount} 项）` : ""}
          </Button>
        </Space>
      </div>

      {readOnlyGroups.length > 0 ? (
        <Card title="只读配置" className="setting-group">
          {readOnlyGroups.map((group) => (
            <div key={group.key} className="readonly-grid">
              {group.items.map((item) => (
                <div key={item.key} className="readonly-item">
                  <div className="readonly-label">{item.description}</div>
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
