param(
  [string]$Platform = "windows",
  [string]$Arch = $env:MAGI_PACKAGE_ARCH
)

$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RootDir = Resolve-Path (Join-Path $ScriptDir "../..")

if ([string]::IsNullOrWhiteSpace($Arch)) {
  $Arch = if ([Environment]::Is64BitOperatingSystem) { "x64" } else { "x86" }
}

$CargoToml = Join-Path $RootDir "Cargo.toml"
$Version = $null
$InWorkspacePackage = $false
foreach ($Line in Get-Content $CargoToml) {
  if ($Line -match '^\[workspace\.package\]') {
    $InWorkspacePackage = $true
    continue
  }
  if ($Line -match '^\[') {
    $InWorkspacePackage = $false
  }
  if ($InWorkspacePackage -and $Line -match '^version\s*=\s*"([^"]+)"') {
    $Version = $Matches[1]
    break
  }
}

if ([string]::IsNullOrWhiteSpace($Version)) {
  throw "无法从 Cargo.toml 读取 workspace 版本号。"
}

$Binary = Join-Path $RootDir "target/release/magi-daemon-app.exe"
$WebDist = Join-Path $RootDir "web/dist"
$WebHtml = Join-Path $WebDist "web.html"

if (-not (Test-Path $Binary -PathType Leaf)) {
  throw "缺少 release daemon 二进制：$Binary"
}

if (-not (Test-Path $WebHtml -PathType Leaf)) {
  throw "缺少前端构建产物：$WebHtml"
}

$DistDir = Join-Path $RootDir "dist"
$PackageName = "magi-$Version-$Platform-$Arch"
$PackageDir = Join-Path $DistDir $PackageName

Remove-Item $PackageDir -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path (Join-Path $PackageDir "resources/web") | Out-Null

Copy-Item $Binary (Join-Path $PackageDir "Magi.exe")
Copy-Item $WebDist (Join-Path $PackageDir "resources/web/dist") -Recurse

if (-not (Test-Path (Join-Path $PackageDir "Magi.exe") -PathType Leaf)) {
  throw "产品包缺少 Magi.exe 入口：$(Join-Path $PackageDir "Magi.exe")"
}

function Get-PeSubsystem {
  param([string]$Path)

  $Bytes = [System.IO.File]::ReadAllBytes($Path)
  if ($Bytes.Length -lt 0x80) {
    throw "PE 文件过短：$Path"
  }

  $PeOffset = [BitConverter]::ToInt32($Bytes, 0x3c)
  if ($PeOffset -lt 0 -or $PeOffset + 92 -ge $Bytes.Length) {
    throw "PE 头偏移非法：$Path"
  }

  if ($Bytes[$PeOffset] -ne 0x50 -or $Bytes[$PeOffset + 1] -ne 0x45 -or $Bytes[$PeOffset + 2] -ne 0 -or $Bytes[$PeOffset + 3] -ne 0) {
    throw "PE 签名非法：$Path"
  }

  $OptionalHeaderOffset = $PeOffset + 24
  return [BitConverter]::ToUInt16($Bytes, $OptionalHeaderOffset + 68)
}

$MagiExe = Join-Path $PackageDir "Magi.exe"
$WindowsGuiSubsystem = 2
$Subsystem = Get-PeSubsystem $MagiExe
if ($Subsystem -ne $WindowsGuiSubsystem) {
  throw "Magi.exe 必须使用 Windows GUI 子系统，当前子系统值：$Subsystem"
}

if (-not (Test-Path (Join-Path $PackageDir "resources/web/dist/web.html") -PathType Leaf)) {
  throw "产品包缺少内置 UI 入口：$(Join-Path $PackageDir "resources/web/dist/web.html")"
}

$ForbiddenEntries = Get-ChildItem $PackageDir -Recurse -Force |
  Where-Object { $_.Name -like "magi-daemon-app*" -or $_.Name -like "start-magi*" }
if ($ForbiddenEntries) {
  $ForbiddenNames = ($ForbiddenEntries | ForEach-Object { $_.FullName }) -join ", "
  throw "产品包不能暴露 magi-daemon-app 或 start-magi 技术入口：$ForbiddenNames"
}

$ZipPath = Join-Path $DistDir "$PackageName.zip"
Remove-Item $ZipPath -Force -ErrorAction SilentlyContinue
Compress-Archive -Path $PackageDir -DestinationPath $ZipPath
Write-Host "已生成 $ZipPath"
