"""media-gateway 内部包。

分层（依赖单向向下）：

    config      环境变量 / .env → Settings
    errors      GatewayError（带 HTTP 状态码与回显文本）
    httpclient  urllib 极简助手（自动读 NO_PROXY，绕过本机代理）
    imaging     出图：a1111 / comfyui 两个后端；workflow 负责节点识别
    speech      出声：sapi / edge-tts 两个引擎
    server      http.server 路由与启动

入口是上一层目录的 main.py（`python main.py`）。
"""
