"""配置：环境变量 → 不可变 Settings（零依赖，仅标准库）。

环境变量（由 MCP 客户端的 `mcp.json` 传进来，也可用本目录的 `.env` 兜底）：
    COMFYUI_URL              ComfyUI API 基址（默认 http://127.0.0.1:8188）
    COMFYUI_WORKFLOW_FILE    API 格式工作流 JSON；相对路径按本目录解析
    COMFYUI_PROMPT_NODE      正向提示词节点 id 覆盖（默认 3）
    COMFYUI_NEGATIVE_NODE    负向提示词节点 id 覆盖（默认 4）
    COMFYUI_SIZE_NODE        尺寸节点 id 覆盖（默认 6）
    COMFYUI_KSAMPLER_NODE    采样器节点 id 覆盖（默认 5）
    COMFYUI_TIMEOUT          单次出图总超时秒（默认 300）

节点 id 只是**可选覆盖**：类型对不上时按 class_type 自动识别兜底（见 workflow.py），
故换一份工作流通常不用改这里。
"""

from __future__ import annotations

import os
import sys
from dataclasses import dataclass
from pathlib import Path


def base_dir() -> Path:
    """配置与相对路径的基准目录。

    源码运行时是本目录（包位于其下的 `comfy/` 子目录）；PyInstaller 冻结后 `__file__`
    位于临时解包目录，必须改用 exe 所在目录，否则工作流文件永远找不到。
    """
    if getattr(sys, "frozen", False):
        return Path(sys.executable).resolve().parent
    return Path(__file__).resolve().parent.parent


BASE_DIR = base_dir()


def load_dotenv(path: str | os.PathLike[str] = ".env") -> None:
    """极简 .env 加载（标准库，零依赖）。进程已存在的环境变量优先，不覆盖。

    MCP 客户端在 `mcp.json` 的 `env` 段里配的值是主来源；这里的 `.env` 只是本机
    手工调试时的兜底。
    """
    full = Path(path)
    if not full.is_absolute():
        full = BASE_DIR / full
    if not full.exists():
        return
    with open(full, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, val = line.split("=", 1)
            key, val = key.strip(), val.strip()
            # 去掉成对的单/双引号。
            if len(val) >= 2 and val[0] == val[-1] and val[0] in ("'", '"'):
                val = val[1:-1]
            if key and key not in os.environ:
                os.environ[key] = val


@dataclass(frozen=True)
class Settings:
    """一次进程内不变的运行配置。"""

    comfyui_url: str
    comfyui_workflow_file: str  # 已解析为绝对路径；空串表示用内置兜底工作流
    comfyui_prompt_node: str
    comfyui_negative_node: str
    comfyui_size_node: str
    comfyui_ksampler_node: str
    timeout: float

    @classmethod
    def from_env(cls) -> "Settings":
        workflow_file = os.getenv("COMFYUI_WORKFLOW_FILE", "").strip()
        # 相对路径按本目录解析，避免"只有在某目录下启动才找得到"的坑。
        if workflow_file and not Path(workflow_file).is_absolute():
            workflow_file = str(BASE_DIR / workflow_file)
        return cls(
            comfyui_url=os.getenv("COMFYUI_URL", "http://127.0.0.1:8188").rstrip("/"),
            comfyui_workflow_file=workflow_file,
            comfyui_prompt_node=os.getenv("COMFYUI_PROMPT_NODE", "3"),
            comfyui_negative_node=os.getenv("COMFYUI_NEGATIVE_NODE", "4"),
            comfyui_size_node=os.getenv("COMFYUI_SIZE_NODE", "6"),
            comfyui_ksampler_node=os.getenv("COMFYUI_KSAMPLER_NODE", "5"),
            timeout=float(os.getenv("COMFYUI_TIMEOUT", "300")),
        )


load_dotenv()  # 必须在 from_env 之前
settings = Settings.from_env()
