import { Button, Card, Checkbox, Modal, Popconfirm, Tag, Typography, message } from "antd";
import { useEffect, useState } from "react";
import { api } from "../api";
import type { IdentityOverview, ProposalView } from "../types";

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
  const [deciding, setDeciding] = useState<string | null>(null);
  const [detailOpen, setDetailOpen] = useState(false);
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
        messageApi.info("本轮无新提案");
      }
      await refresh();
    } else {
      messageApi.error(result.error ?? "触发失败");
    }
    setTriggering(false);
  };

  const pendingCount = overview?.pendingCount ?? proposals.length;
  const identityText = overview?.text?.trim();

  return (
    <>
      {contextHolder}
      <Typography.Title level={2} className="page-title">
        首页
      </Typography.Title>

      <Card
        className="identity-card"
        title="身份演化"
        extra={
          pendingCount > 0 ? (
            <Tag color="orange" bordered={false}>
              {pendingCount} 条待审批
            </Tag>
          ) : null
        }
        actions={[
          <Button
            key="detail"
            type="primary"
            onClick={() => setDetailOpen(true)}
          >
            查看详情
          </Button>,
        ]}
      >
        {identityText ? (
          <Typography.Paragraph
            className="identity-summary"
            ellipsis={{ rows: 4, expandable: false }}
          >
            {identityText}
          </Typography.Paragraph>
        ) : (
          <Typography.Text type="secondary">还没有身份</Typography.Text>
        )}
        <div className="identity-meta">
          {overview?.version !== null && overview !== null ? (
            <span>v{overview.version}</span>
          ) : null}
        </div>
      </Card>

      <Modal
        title="身份详情"
        open={detailOpen}
        onCancel={() => setDetailOpen(false)}
        footer={null}
        width={560}
      >
        <div className="identity-full">
          {identityText ?? "还没有身份"}
        </div>
        <div className="identity-actions">
          <span className="mono">{overview?.version !== null && overview !== null ? `v${overview.version}` : ""}</span>
          <Button size="small" loading={triggering} onClick={() => void trigger()}>
            手动演化
          </Button>
          {overview && overview.version !== null ? (
            <Popconfirm title="确定回滚到上一版本吗？" onConfirm={() => void rollback()}>
              <Button danger size="small">
                回滚
              </Button>
            </Popconfirm>
          ) : null}
        </div>

        {proposals.length > 0 ? (
          <div className="proposal-list">
            <Typography.Title level={5}>待审批提案</Typography.Title>
            {proposals.map((proposal) => (
              <Card key={proposal.id} className="proposal-card" size="small">
                <div className="proposal-head">
                  <Tag bordered={false}>{proposal.fieldKey}</Tag>
                  <Tag color={riskColor[proposal.riskLevel]} bordered={false}>
                    {riskText[proposal.riskLevel]}
                  </Tag>
                  <span className="mono">{proposal.confidence}</span>
                </div>
                <div className="proposal-change">
                  {proposal.beforeValue ? (
                    <Typography.Text delete type="secondary">
                      {proposal.beforeValue}
                    </Typography.Text>
                  ) : null}
                  <Typography.Text strong>{proposal.afterValue}</Typography.Text>
                </div>
                <Typography.Paragraph type="secondary" className="proposal-reason">
                  {proposal.reason}
                </Typography.Paragraph>
                <div className="proposal-ops">
                  <Button
                    type="primary"
                    size="small"
                    loading={deciding === proposal.id}
                    onClick={() => setConsentModal(proposal)}
                  >
                    批准
                  </Button>
                  <Popconfirm
                    title="确定拒绝这条提案吗？"
                    onConfirm={() => void reject(proposal)}
                  >
                    <Button danger size="small" loading={deciding === proposal.id}>
                      拒绝
                    </Button>
                  </Popconfirm>
                </div>
              </Card>
            ))}
          </div>
        ) : null}
      </Modal>

      <Modal
        title="确认应用身份变更"
        open={consentModal !== null}
        onCancel={() => setConsentModal(null)}
        footer={null}
      >
        {consentModal && (
          <div className="consent-body">
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
          </div>
        )}
      </Modal>
    </>
  );
}
