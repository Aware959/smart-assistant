// 浏览器预览用 mock 桥：不需要 zhiwei-service / media-gateway 就能看 UI。
// 正式入口 index.html 走真实 Tauri invoke，不加载本文件。
(() => {
  const runtimeSettingsSchema = [
    ["intent_enabled", "boolean", "主动开口总开关", true],
    ["intent_min_minutes", "integer", "主动开口随机间隔下限（分钟）", 45],
    ["intent_max_minutes", "integer", "主动开口随机间隔上限（分钟）", 180],
    ["intent_quiet_hours", "string", "安静时段，格式 HH-HH", "23-7"],
    ["intent_min_silence_hours", "integer", "用户最后发言后至少静默多少小时", 2],
    ["intent_max_idle_days", "integer", "用户沉默超过多少天后停止主动联系", 7],
    ["memory_recall_min_similarity", "number", "记忆召回最低余弦相似度", 0.79],
    ["memory_dedup_min_similarity", "number", "记忆去重最低余弦相似度", 0.92],
    ["media_gateway_enabled", "boolean", "图片/语音生成工具总开关", true],
    ["media_gateway_timeout", "integer", "媒体生成超时（秒）", 120],
    ["voice_transcribe_enabled", "boolean", "入站语音转写总开关", true],
    ["voice_transcribe_timeout", "integer", "单次语音转写超时（秒）", 180],
    ["voice_transcribe_max_secs", "integer", "允许转写的语音时长上限（秒）", 120],
    ["identity_evolution_enabled", "boolean", "身份演化后台任务总开关", false],
    ["identity_evolution_interval_hours", "integer", "身份演化检查周期（小时）", 24],
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
            data: runtimeSettingsSchema.map(([key, kind, description, d]) => ({
              key,
              kind,
              description,
              default: d,
              current: d,
              hot_reload: true,
              restart_required: false,
            })),
            error: null,
          };
        case "update_settings":
          revision += 1;
          Object.assign(runtimeValues, args.changes ?? {});
          return { ok: true, data: { revision, values: { ...runtimeValues } }, error: null };
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
