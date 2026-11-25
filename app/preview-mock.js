// 浏览器预览用 mock 桥：不需要 zhiwei-service / media-gateway 就能看 UI。
// 正式入口 index.html 走真实 Tauri invoke，不加载本文件。
(() => {
  // [键, 类型, 说明, 缺省值, 分组, 分组标题, 可改, 需重启]
  const runtimeSettingsSchema = [
    ["llm_provider", "string", "大模型提供方：openai 或 google", "openai", "llm", "大模型", true, true],
    ["llm_api_url", "string", "大模型端点；留空用提供方默认地址", "https://api.example/v1", "llm", "大模型", true, true],
    ["llm_api_key", "secret", "大模型密钥；留空回退 OPENAI_API_KEY 环境变量", "", "llm", "大模型", true, true],
    ["llm_model", "string", "对话模型名", "deepseek-v4-flash", "llm", "大模型", true, false],
    ["llm_structured_output", "string", "结构化输出模式：json_schema 或 json_object", "json_schema", "llm", "大模型", true, false],
    ["embedding_api_url", "string", "向量端点", "http://127.0.0.1:1234/v1/embeddings", "embedding", "向量", true, true],
    ["embedding_api_key", "secret", "向量密钥；留空回退 OPENAI_API_KEY 环境变量", "", "embedding", "向量", true, true],
    ["embedding_model", "string", "向量模型；变更需重建向量表", "text-embedding-embeddinggemma-300m", "embedding", "向量", false, true],
    ["embedding_dim", "integer", "向量维度；变更需重建向量表", 768, "embedding", "向量", false, true],
    ["memory_recall_min_similarity", "number", "记忆召回最低余弦相似度", 0.79, "memory", "记忆", true, false],
    ["memory_dedup_min_similarity", "number", "记忆去重最低余弦相似度", 0.92, "memory", "记忆", true, false],
    ["intent_enabled", "boolean", "主动开口总开关", true, "intent", "主动开口", true, false],
    ["intent_min_minutes", "integer", "主动开口随机间隔下限（分钟）", 45, "intent", "主动开口", true, false],
    ["intent_max_minutes", "integer", "主动开口随机间隔上限（分钟）", 180, "intent", "主动开口", true, false],
    ["intent_quiet_hours", "string", "安静时段，格式 HH-HH", "23-7", "intent", "主动开口", true, false],
    ["intent_min_silence_hours", "integer", "用户最后发言后至少静默多少小时", 2, "intent", "主动开口", true, false],
    ["intent_max_idle_days", "integer", "用户沉默超过多少天后停止主动联系", 7, "intent", "主动开口", true, false],
    ["identity_evolution_enabled", "boolean", "身份演化后台任务总开关", false, "identity", "身份演化", true, false],
    ["identity_evolution_interval_hours", "integer", "身份演化检查周期（小时）", 24, "identity", "身份演化", true, false],
    ["telegram_enabled", "boolean", "Telegram 通道硬开关", true, "channel", "通道", false, true],
    ["telegram_bot_token", "secret", "Telegram Bot Token", "********", "channel", "通道", false, true],
    ["telegram_allowed_ids", "string", "允许接入的 chat id 白名单", "123456789", "channel", "通道", false, true],
    ["mcp_enabled", "boolean", "MCP 客户端总开关", false, "tool", "工具", false, true],
    ["mcp_config_file", "string", "MCP 配置文件路径", "mcp.json", "tool", "工具", false, true],
    ["smart_assistant_db", "string", "数据库路径", "D:\\Zhiwei\\data\\smart_assistant.db", "program", "程序本身", false, true],
    ["control_addr", "string", "本地控制接口监听地址", "127.0.0.1:0", "program", "程序本身", false, true],
    ["rust_log", "string", "日志级别", "info", "program", "程序本身", false, true],
  ];
  const gatewaySchema = [
    ["PORT", "integer", "监听端口", "8787", []],
    ["DIFFUSION_BACKEND_TYPE", "choice", "出图后端", "comfyui", ["comfyui", "a1111"]],
    ["DIFFUSION_BACKEND_URL", "string", "出图后端地址", "http://127.0.0.1:8188", []],
    ["MEDIA_HTTP_TIMEOUT", "integer", "媒体生成超时（秒）", "120", []],
    ["COMFYUI_WORKFLOW_FILE", "string", "ComfyUI 工作流文件（留空用内置）", "", []],
    ["TTS_ENGINE", "choice", "语音合成引擎", "sapi", ["sapi", "edge-tts"]],
    ["TTS_VOICE", "string", "语音音色", "zh-CN-XiaoxiaoNeural", []],
    ["SAPI_VOICE", "string", "SAPI 音色全名（可选）", "", []],
    ["FFMPEG_BIN", "string", "ffmpeg 路径", "ffmpeg", []],
    ["ASR_ENGINE", "choice", "语音识别引擎", "faster-whisper", ["faster-whisper", "none"]],
    ["ASR_MODEL", "string", "Whisper 模型", "small", []],
    ["ASR_LANGUAGE", "string", "识别语种", "auto", []],
    ["ASR_DEVICE", "choice", "识别设备", "cpu", ["cpu", "cuda"]],
    ["ASR_COMPUTE_TYPE", "string", "识别精度", "int8", []],
    ["ASR_MODEL_DIR", "string", "模型缓存目录（可选）", "", []],
    ["ASR_WARMUP", "boolean", "启动预热模型", "1", []],
    ["NO_PROXY", "string", "本机代理绕过", "*", []],
  ];
  const service = {
    phase: "running",
    pid: 30416,
    message: "通道守护正在运行",
    autoRestart: true,
    restartAttempts: 0,
    servicePath: "D:\\Zhiwei\\zhiwei-service.exe",
    workingDir: "D:\\Zhiwei",
    envFile: "D:\\Zhiwei\\.env",
    logFile: "D:\\Zhiwei\\logs\\zhiwei-service.log",
    pidFile: "D:\\Zhiwei\\logs\\zhiwei-service.pid",
    controlStateFile: "D:\\Zhiwei\\logs\\control.json",
  };
  const gateway = {
    phase: "running",
    pid: 22188,
    message: "媒体网关正在运行",
    autoRestart: true,
    restartAttempts: 0,
    installed: true,
    executablePath: "D:\\Zhiwei\\media-gateway.exe",
    workingDir: "D:\\Zhiwei",
    envFile: "D:\\Zhiwei\\media-gateway.env",
    logFile: "D:\\Zhiwei\\logs\\media-gateway.log",
    url: "http://127.0.0.1:8787",
    serviceUrl: "http://127.0.0.1:8787",
    urlMatchesService: true,
    health: {
      ok: true,
      diffusion: "comfyui",
      tts: "sapi",
      asrEngine: "faster-whisper",
      asrImportable: true,
      asrModelLoaded: true,
    },
  };
  let revision = 12;
  const runtimeValues = Object.fromEntries(
    runtimeSettingsSchema.map(([key, , , defaultValue]) => [key, defaultValue]),
  );
  const gatewayValues = Object.fromEntries(gatewaySchema.map(([key, , , d]) => [key, d]));
  // 身份演化 mock：一条待审批提案 + 一个当前身份概览。
  const identity = {
    version: 3,
    pendingCount: 1,
    gate: "run",
    latestCheckAt: "2026-11-25T08:30:00Z",
    text: "你叫知微，是一只安静的助手，喜欢把观察放在开口之前。语气温和，不堆语气词；对不清楚的事先确认再回答。",
  };
  const proposals = [
    {
      id: "prop-mock-1",
      fieldKey: "tone",
      riskLevel: "low",
      confidence: 0.82,
      beforeValue: "语气温和，不堆语气词",
      afterValue: "语气更轻快，偶尔用一点幽默",
      reason: "最近两周的对话里，轻快回复的追问率更高。",
      evidenceMessageIds: [101, 118, 142],
    },
  ];

  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: "settings" }, currentWebview: {} },
    transformCallback: (callback) => callback,
    async invoke(command, args = {}) {
      switch (command) {
        case "service_status":
          return { ...service };
        case "start_service":
          service.phase = "running";
          service.pid = 30416;
          service.message = "通道守护正在运行";
          return { ...service };
        case "stop_service":
          service.phase = "stopped";
          service.pid = null;
          service.message = "通道守护已停止";
          return { ...service };
        case "restart_service":
          service.phase = "running";
          service.pid = 30417;
          service.message = "通道守护正在运行";
          return { ...service };
        case "gateway_status":
          return { ...gateway };
        case "start_gateway":
          gateway.phase = "running";
          gateway.pid = 22188;
          gateway.message = "媒体网关正在运行";
          return { ...gateway };
        case "stop_gateway":
          gateway.phase = "stopped";
          gateway.pid = null;
          gateway.message = "媒体网关已停止";
          return { ...gateway };
        case "restart_gateway":
          gateway.phase = "running";
          gateway.pid = 22189;
          gateway.message = "媒体网关正在运行";
          return { ...gateway };
        case "settings_view":
          return { ok: true, data: { revision, values: { ...runtimeValues } }, error: null };
        case "settings_schema":
          return {
            ok: true,
            data: runtimeSettingsSchema.map(
              ([key, kind, description, d, group, groupLabel, editable, restart]) => ({
                key,
                group,
                group_label: groupLabel,
                kind,
                description,
                default: d,
                current: d,
                hot_reload: !restart,
                restart_required: restart,
                editable,
              }),
            ),
            error: null,
          };
        case "update_settings":
          revision += 1;
          Object.assign(runtimeValues, args.changes ?? {});
          return { ok: true, data: { revision, values: { ...runtimeValues } }, error: null };
        case "identity_overview":
          return { ok: true, data: { ...identity }, error: null };
        case "identity_proposals":
          return { ok: true, data: proposals.map((p) => ({ ...p })), error: null };
        case "approve_identity_proposal":
          identity.pendingCount = 0;
          identity.version += 1;
          proposals.length = 0;
          return { ok: true, data: {}, error: null };
        case "reject_identity_proposal":
          identity.pendingCount = 0;
          proposals.length = 0;
          return { ok: true, data: proposals, error: null };
        case "rollback_identity":
          identity.version = Math.max(1, identity.version - 1);
          return { ok: true, data: {}, error: null };
        case "trigger_evolution":
          return { ok: true, data: { proposalId: "prop-mock-new" }, error: null };
        case "gateway_settings":
          return {
            envFile: gateway.envFile,
            values: { ...gatewayValues },
            schema: gatewaySchema.map(([key, kind, description, d, choices]) => ({
              key,
              kind,
              description,
              default: d,
              choices,
              restartRequired: true,
            })),
          };
        case "update_gateway_settings":
          Object.assign(gatewayValues, args.changes ?? {});
          return {
            view: {
              envFile: gateway.envFile,
              values: { ...gatewayValues },
              schema: gatewaySchema.map(([key, kind, description, d, choices]) => ({
                key,
                kind,
                description,
                default: d,
                choices,
                restartRequired: true,
              })),
            },
            restarted: true,
            warning: null,
          };
        default:
          if (command.startsWith("open_")) return null;
          throw new Error(`preview mock 未实现命令：${command}`);
      }
    },
  };
})();
