# repo-guard：仓库治理守卫（确定性检查，不调用模型）
# 用法：repo_guard.ps1 <事件名>；输入为 Claude Code hook 的 stdin JSON。
# 事件：PreToolUse（路径保护 + 新文件命名）、Stop（fmt + check 基线）。
# 成功静默退出 0；失败输出简短原因并以退出码 2 阻断。
param(
    [string]$Event
)

$ErrorActionPreference = 'Continue'

# 读 hook 输入（stdin JSON），失败时静默放行——守卫绝不能卡住正常工作。
# 注意不能用 [Console]::In.ReadToEnd()：PowerShell 5.1 在空管道下它会抛异常并
# 让整个脚本以 rc=0 假成功退出（阻断逻辑被跳过）。$input 在任何宿主下都安全；
# param 不能加 Mandatory——否则 stdin 文本会被自动绑定到 $Event 引发报错。
$raw = try { ($input | Out-String) } catch { $null }
if ($raw -and $raw.Trim()) {
    try { $payload = $raw | ConvertFrom-Json } catch { $payload = $null }
} else { $payload = $null }

if (-not $Event) {
    if ($args.Count -ge 1) { $Event = $args[0] } else { exit 0 }
}

function Fail([string]$reason) {
    Write-Output $reason
    exit 2
}

# 仓库根 = 本脚本向上两级（.claude/hooks/ -> 仓库根）
$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)

switch ($Event) {

    'PreToolUse' {
        # 只看写文件类工具。
        $tool = $payload.tool_name
        if ($tool -notin @('Write', 'Edit', 'NotebookEdit')) { exit 0 }

        $path = $payload.tool_input.file_path
        if (-not $path) { exit 0 }
        # 归一为相对路径（正斜杠），只拦仓库内路径。
        $rel = $path.Replace('\', '/')
        if ($rel -match '^[A-Za-z]:/') {
            $rootFwd = ($repoRoot.Replace('\', '/')).TrimEnd('/') + '/'
            if (-not $rel.StartsWith($rootFwd, [StringComparison]::OrdinalIgnoreCase)) { exit 0 }
            $rel = $rel.Substring($rootFwd.Length)
        }

        # 1) 路径保护：iLink 端不主动改（AGENTS.md 主线约定）。
        $protected = @('backend/src/channels/ilink/')
        foreach ($p in $protected) {
            if ($rel.ToLower().StartsWith($p)) {
                Fail ("[repo-guard] `{0}` 属于 iLink 端，主线开发期间不主动修改。确需修改请让用户明确点名。" -f $rel)
            }
        }

        # 2) 新增 Rust 源文件命名：snake_case。
        if ($rel -match '\.rs$') {
            $name = [IO.Path]::GetFileNameWithoutExtension($rel)
            if ($name -cnotmatch '^[a-z0-9_]+$' -and $name -ne 'lib' -and $name -ne 'main') {
                Fail ("[repo-guard] 新 Rust 文件 `{0}` 不符合 snake_case 命名约定。" -f $rel)
            }
        }

        # 3) docs/archive/ 目录名必须全为英文单词。
        if ($rel -match '^docs/archive/') {
            $rest = $rel.Substring('docs/archive/'.Length)
            foreach ($seg in ($rest -split '/' | Select-Object -First 1)) {
                if ($seg -and ($seg -match '[^\x00-\x7F]' -or $seg -match '[0-9_]')) {
                    Fail ("[repo-guard] `docs/archive/{0}` 目录名应为英文单词。" -f $seg)
                }
            }
        }
        exit 0
    }

    'Stop' {
        # 一轮结束时：工作区有后端 Rust 改动才跑基线，成功静默。
        # 用 status --porcelain 而非 diff：未跟踪的新文件也要覆盖。
        $changed = git -C $repoRoot status --porcelain -- backend 2>$null |
            Where-Object { $_ -match '\.rs\s*$' }
        if (-not $changed -or @($changed).Count -eq 0) { exit 0 }

        Push-Location (Join-Path $repoRoot 'backend')
        try {
            $fmt = cargo fmt --all -- --check 2>&1
            if ($LASTEXITCODE -ne 0) {
                Fail "[repo-guard] cargo fmt --check 未通过：请先跑 cargo fmt --all"
            }
            $check = cargo check --all-targets 2>&1
            if ($LASTEXITCODE -ne 0) {
                Fail "[repo-guard] cargo check --all-targets 失败：`n$($check | Select-Object -First 15)"
            }
            $warn = @($check) | Where-Object { $_ -match '^warning' }
            if ($warn.Count -gt 0) {
                Fail ("[repo-guard] cargo check 有 {0} 条警告（基线为零警告）：" -f $warn.Count)
            }
        } finally { Pop-Location }
        exit 0
    }

    default { exit 0 }
}
