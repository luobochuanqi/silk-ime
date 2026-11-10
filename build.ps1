# Silk IME 构建与验证（Windows PowerShell）
$ErrorActionPreference = 'Stop'

Write-Host '== Go 构建 =='
go build ./...
go vet ./...
go test ./...

Write-Host '== Rust 协议 =='
cargo test --manifest-path native/protocol/Cargo.toml

Write-Host '全部通过'
