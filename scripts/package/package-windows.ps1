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

$ZipPath = Join-Path $DistDir "$PackageName.zip"
Remove-Item $ZipPath -Force -ErrorAction SilentlyContinue
Compress-Archive -Path $PackageDir -DestinationPath $ZipPath
Write-Host "已生成 $ZipPath"
