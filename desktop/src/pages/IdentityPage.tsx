import {
  Alert,
  Button,
  Card,
  Checkbox,
  Col,
  Empty,
  Modal,
  Popconfirm,
  Row,
  Space,
  Statistic,
  Tag,
  Typography,
  message,
} from "antd";
import { useEffect, useState } from "react";
import { api } from "../api";
import type { IdentityOverview, ProposalView } from "../types";

const gateText: Record<IdentityOverview["gate"], string> = {
  run: "可运行",
  pending_limit: "待审批已满",
  global_cooldown: "全局冷却中",
  disabled: "未启用",
};

const riskColor: Record<ProposalView["riskLevel"], string> = {
  low: "green",
  medium: "orange",
  high: "red",
};

const riskText: Record<ProposalView["riskLevel"], string> = {
  low: "低风险",
  medium: "中风险",
  high: "高风险",
};

export default function IdentityPage() {
  const [overview, setOverview] = useState<IdentityOverview | null>(null);
  const [proposals, setProposals] = useState<ProposalView[]>([]);
  const [loading, setLoading] = useState(true);
  const [deciding, setDeciding] = useState<string | null>(null);
  const [consentModal, setConsentModal] = useState<ProposalView | null>(null);
  const [triggering, setTriggering] = useState(false);
  const [messageApi, contextHolder] = message.useMessage();

  const refresh = async () => {
    const [overviewResult, proposalsResult] = await Promise.all([
      api.identityOverview(),
      api.identityProposals(50),
    ]);
    if (overviewResult.ok && overviewResult.data) setOverview(overviewResult.data);
    if (proposalsResult.ok && proposalsResult.data) setProposals(proposalsResult.data);
    setLoading(false);
  };

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), 30_000);
    return () => window.clearInterval(timer);
  }, []);

  const approve = async (proposal: ProposalView) => {
    setDeciding(proposal.id);
    const result = await api.approveProposal(proposal.id, true);
    if (result.ok) {
      messageApi.success("提案已批准并应用");
      setConsentModal(null);
      await refresh();
    } else {
      messageApi.error(result.error ?? "操作失败");
    }
    setDeciding(null);
  };

  const reject = async (proposal: ProposalView) => {
    setDeciding(proposal.id);
    const result = await api.rejectProposal(proposal.id);
    if (result.ok) {
      messageApi.success("提案已拒绝");
      await refresh();
    } else {
      messageApi.error(result.error ?? "操作失败");
    }
    setDeciding(null);
  };

  const rollback = async () => {
    const result = await api.rollbackIdentity();
    if (result.ok) {
      messageApi.success("已回滚到上一版本");
      await refresh();
    } else {
      messageApi.error(result.error ?? "回滚失败");
    }
  };

  const trigger = async () => {
    setTriggering(true);
    const result = await api.triggerEvolution();
    if (result.ok) {
      const data = result.data as Record<string, unknown> | null;
      if (data?.proposalId) {
        messageApi.success("演化已生成新提案");
      } else {
        messageApi.info("本轮无新提案（证据不足或模型未识别到变化）");
      }
      await refresh();
    } else {
      messageApi.error(result.error ?? "触发失败");
    }
    setTriggering(false);
  };

  if (loading) return <Card loading />;

  return (
    <Space direction="vertical" size={20} className="full-width">
      {contextHolder}
      <Typography.Title level={3} className="page-title">
        身份演化
      </Typography.Title>

      <Row gutter={16}>
        <Col span={6}>
          <Card>
            <Statistic title="身份版本" value={overview?.version ?? "—"} />
          </Card>
        </Col>
        <Col span={6}>
          <Card>
            <Statistic title="待审批" value={overview?.pendingCount ?? 0} />
          </Card>
        </Col>
        <Col span={6}>
          <Card>
            <Statistic title="演化状态" value={overview ? gateText[overview.gate] : "—"} />
          </Card>
        </Col>
        <Col span={6}>
          <Card>
            <Statistic
              title="上次检查"
              value={overview?.latestCheckAt ? new Date(overview.latestCheckAt).toLocaleString() : "从未"}
            />
          </Card>
        </Col>
      </Row>

      <Card
        title="当前身份"
        extra={
          <Space>
            <Button
              size="small"
              loading={triggering}
              disabled={overview?.gate !== "run"}
              onClick={() => void trigger()}
            >
              手动演化
            </Button>
            {overview && overview.version !== null && (
              <Popconfirm title="确定回滚到上一版本吗？" onConfirm={() => void rollback()}>
                <Button danger size="small">
                  回滚
                </Button>
              </Popconfirm>
            )}
          </Space>
        }
      >
        <Typography.Paragraph className="mono" style={{ whiteSpace: "pre-wrap" }}>
          {overview?.text ?? "加载中…"}
        </Typography.Paragraph>
      </Card>

      <Card title="待审批提案">
        {proposals.length === 0 ? (
          <Empty description="暂无待审批提案" />
        ) : (
          proposals.map((proposal) => (
            <Card
              key={proposal.id}
              type="inner"
              style={{ marginBottom: 12 }}
              title={
                <Space>
                  <Tag>{proposal.fieldKey}</Tag>
                  <Tag color={riskColor[proposal.riskLevel]}>{riskText[proposal.riskLevel]}</Tag>
                  <Typography.Text type="secondary">置信度 {proposal.confidence}</Typography.Text>
                </Space>
              }
              actions={[
                <Button
                  key="approve"
                  type="primary"
                  size="small"
                  loading={deciding === proposal.id}
                  onClick={() => setConsentModal(proposal)}
                >
                  批准
                </Button>,
                <Popconfirm
                  key="reject"
                  title="确定拒绝这条提案吗？"
                  onConfirm={() => void reject(proposal)}
                >
                  <Button danger size="small" loading={deciding === proposal.id}>
                    拒绝
                  </Button>
                </Popconfirm>,
              ]}
            >
              <Space direction="vertical" style={{ width: "100%" }}>
                {proposal.beforeValue && (
                  <div>
                    <Typography.Text type="secondary">当前值：</Typography.Text>
                    <Typography.Text delete>{proposal.beforeValue}</Typography.Text>
                  </div>
                )}
                <div>
                  <Typography.Text type="secondary">新值：</Typography.Text>
                  <Typography.Text strong>{proposal.afterValue}</Typography.Text>
                </div>
                <div>
                  <Typography.Text type="secondary">理由：</Typography.Text>
                  {proposal.reason}
                </div>
                <Alert
                  type="info"
                  message={`证据消息 ID：${proposal.evidenceMessageIds.join(", ") || "无"}`}
                />
              </Space>
            </Card>
          ))
        )}
      </Card>

      <Modal
        title="确认应用身份变更"
        open={consentModal !== null}
        onCancel={() => setConsentModal(null)}
        footer={null}
      >
        {consentModal && (
          <Space direction="vertical" style={{ width: "100%" }}>
            <div>
              <Typography.Text type="secondary">字段：</Typography.Text>
              {consentModal.fieldKey}
            </div>
            <div>
              <Typography.Text type="secondary">新值：</Typography.Text>
              <Typography.Text strong>{consentModal.afterValue}</Typography.Text>
            </div>
            <Checkbox checked disabled>
              我确认同意此身份变更
            </Checkbox>
            <Button
              type="primary"
              block
              loading={deciding === consentModal.id}
              onClick={() => void approve(consentModal)}
            >
              确认应用
            </Button>
          </Space>
        )}
      </Modal>
    </Space>
  );
}
