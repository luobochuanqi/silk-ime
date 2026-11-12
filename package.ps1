# Silk IME 安装包打包（Inno Setup，规格见 docs/design/foundation/07）
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$BuildDir,
    [string]$OutDir
)
$ErrorActionPreference = 'Stop'
if (-not $BuildDir) { $BuildDir = Join-Path $PSScriptRoot '.tmp\build' }
if (-not $OutDir) { $OutDir = Join-Path $PSScriptRoot '.tmp\dist' }

# ISCC 优先取 PATH，其次默认安装位置（x86 / 本机 / 当前用户）
$ISCC = $null
$cmd = Get-Command ISCC.exe -ErrorAction SilentlyContinue
if ($cmd) { $ISCC = $cmd.Source }
if (-not $ISCC) {
    foreach ($p in @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe", "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe")) {
        if (Test-Path $p) { $ISCC = $p; break }
    }
}
if (-not $ISCC) { throw '未找到 ISCC.exe，请安装 Inno Setup 6' }

# 产物齐备才打包（build.ps1 的产出契约）
foreach ($f in 'silk-tsftip.dll', 'silkhost.exe', 'silkui.dll', 'dict\silk.dict') {
    if (-not (Test-Path (Join-Path $BuildDir $f))) { throw "缺少产物 $f，请先运行 .\build.ps1" }
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
& $ISCC "/DAppVersion=$Version" "/DBuildDir=$BuildDir" "/O$OutDir" "/FSilk-Setup-$Version" (Join-Path $PSScriptRoot 'installer\setup.iss')
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# 发布附校验和（07 签名与信任）；走 BCL，不依赖宿主的 Get-FileHash
$lines = foreach ($f in Get-ChildItem $OutDir -Filter '*.exe') {
    $hash = [System.Security.Cryptography.SHA256]::Create().ComputeHash([System.IO.File]::ReadAllBytes($f.FullName))
    '{0}  {1}' -f [System.BitConverter]::ToString($hash).Replace('-', '').ToLower(), $f.Name
}
$lines | Set-Content (Join-Path $OutDir 'SHA256SUMS.txt') -Encoding ascii

Write-Host "安装包 → $OutDir\Silk-Setup-$Version.exe"
