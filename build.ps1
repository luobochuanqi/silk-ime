# Silk IME 构建与验证（Windows PowerShell）
$ErrorActionPreference = 'Stop'
$Out = Join-Path $PSScriptRoot '.tmp\build'

# 仓库可能位于网络挂载（如 SSHFS），Windows 无法从其上执行映像；cargo 的
# 过程宏、构建脚本、测试与本地 exe 都要执行映像，故 target 目录放本地磁盘。
# 显式传 --target-dir 而非依赖环境变量：构建环境的继承行为不可靠。
$Target = Join-Path $env:LOCALAPPDATA 'silk-ime-target'

# 原生命令失败不抛错，包装后逐个检查退出码
function Step([scriptblock]$Cmd) {
    & $Cmd
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

Write-Host '== Go 构建与测试 =='
Step { go build ./... }
Step { go vet ./... }
Step { go test ./... }

Write-Host '== Rust 构建与测试 =='
Step { cargo test --manifest-path native/Cargo.toml --target-dir $Target }
Step { cargo build --release --manifest-path native/Cargo.toml --target-dir $Target }

Write-Host '== 产出到 .tmp\build =='
New-Item -ItemType Directory -Force -Path (Join-Path $Out 'dict') | Out-Null
Step { go build -o (Join-Path $Out 'silkhost.exe') ./cmd/silkhost }
Step { go build -o (Join-Path $Out 'silkdict.exe') ./cmd/silkdict }
# 产物位于网络挂载时无法直接执行，经 go run 在本地临时目录运行
Step { go run ./cmd/silkdict build -o (Join-Path $Out 'dict\silk.dict') dict\source.tsv }
Copy-Item (Join-Path $Target 'release\silkui.dll') $Out -Force
# cargo 将 crate 名的连字符规范为下划线，产物对外仍用设计命名
Copy-Item (Join-Path $Target 'release\silk_tsftip.dll') (Join-Path $Out 'silk-tsftip.dll') -Force

Write-Host "全部通过 → $Out"
