import { Button, Card, List, Space, Typography } from "antd";
import { useEffect, useState } from "react";
import { api } from "../api";
import type { ServiceStatus } from "../types";

export default function FilesPage() {
  const [service, setService] = useState<ServiceStatus | null>(null);

  useEffect(() => {
    api.serviceStatus().then(setService).catch(() => setService(null));
  }, []);

  const rows = [
    {
      label: "通道程序",
      value: service?.servicePath,
      action: "open_data_folder",
      text: "打开目录",
    },
    { label: "控制状态", value: service?.controlStateFile },
    {
      label: "通道配置",
      value: service?.envFile,
      action: "open_env_file_command",
      text: "编辑",
    },
    {
      label: "通道日志",
      value: service?.logFile,
      action: "open_log_file",
      text: "查看",
    },
  ];

  return (
    <Space direction="vertical" size={20} className="full-width">
      <Typography.Title level={3} className="page-title">
        本机文件
      </Typography.Title>
      <Card>
        <List
          dataSource={rows}
          renderItem={(row) => (
            <List.Item
              actions={
                row.action
                  ? [
                      <Button
                        key={row.action}
                        size="small"
                        onClick={() => void api.openFile(row.action!)}
                      >
                        {row.text}
                      </Button>,
                    ]
                  : undefined
              }
            >
              <List.Item.Meta
                title={row.label}
                description={<span className="mono">{row.value ?? "—"}</span>}
              />
            </List.Item>
          )}
        />
      </Card>
      <Typography.Text type="secondary">
        关闭窗口仅隐藏到托盘；右键托盘图标可快速重启或退出。
      </Typography.Text>
    </Space>
  );
}
