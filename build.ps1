# Silk IME 构建与验证（Windows PowerShell）
$ErrorActionPreference = 'Stop'
$Out = Join-Path $PSScriptRoot '.tmp\build'

Write-Host '== Go 构建与测试 =='
go build ./...
go vet ./...
go test ./...

Write-Host '== Rust 构建与测试 =='
cargo test --manifest-path native/Cargo.toml
cargo build --release --manifest-path native/Cargo.toml

Write-Host '== 产出到 .tmp\build =='
New-Item -ItemType Directory -Force -Path (Join-Path $Out 'dict') | Out-Null
go build -o (Join-Path $Out 'silkhost.exe') ./cmd/silkhost
go build -o (Join-Path $Out 'silkdict.exe') ./cmd/silkdict
& (Join-Path $Out 'silkdict.exe') build -o (Join-Path $Out 'dict\silk.dict') dict\source.tsv
Copy-Item (Join-Path $PSScriptRoot 'native\target\release\silkui.dll') $Out -Force
# cargo 将 crate 名的连字符规范为下划线，产物对外仍用设计命名
Copy-Item (Join-Path $PSScriptRoot 'native\target\release\silk_tsftip.dll') (Join-Path $Out 'silk-tsftip.dll') -Force

Write-Host "全部通过 → $Out"
