# 发布脚本：把本仓库推到 GitHub，并可直接发 Release
#
# 用法：
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\publish.ps1 -User <你的GitHub用户名>
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\publish.ps1 -User FYWanye -Repo JongyeolModsI18n
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\publish.ps1 -User FYWanye -Release
#
# 做什么：
#   1. 校验工作区干净（有未提交改动会提示）
#   2. 设置 origin 远程（已存在则改写）
#   3. 确保分支为 main
#   4. 推送（首次用 -u 建立跟踪）
#   5. 加 -Release 时：构建产物并创建 GitHub Release（需要 gh CLI）
#
# 说明：脚本不会替你创建远程仓库。请先在 GitHub 网页上新建**空仓库**（不要勾选 README /
#       .gitignore / LICENSE），或安装 gh CLI 后由脚本提示创建。

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$User,
    [string]$Repo = 'JongyeolModsI18n',
    [string]$Tag,
    [switch]$Release,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$workspace = $PSScriptRoot
if (-not $workspace) { $workspace = (Get-Location).Path }
Set-Location $workspace

if (-not (Test-Path (Join-Path $workspace '.git'))) { throw "当前目录不是 git 仓库：$workspace" }

# --- 1) 工作区状态 -----------------------------------------------------------
$dirty = git status --porcelain
if ($dirty) {
    Write-Warning "存在未提交的改动，请先 git add / git commit 后再发布："
    $dirty | ForEach-Object { Write-Host "  $_" }
    throw '工作区不干净，已中止。'
}

# --- 2) 远程 ----------------------------------------------------------------
$url = "https://github.com/$User/$Repo.git"
$existing = git remote
if ($existing -contains 'origin') { git remote set-url origin $url } else { git remote add origin $url }
Write-Host "origin → $url" -ForegroundColor Cyan

# --- 3) 分支 ----------------------------------------------------------------
$branch = (git rev-parse --abbrev-ref HEAD).Trim()
if ($branch -ne 'main') {
    Write-Host "当前分支为 $branch，重命名为 main" -ForegroundColor Yellow
    git branch -M main
    $branch = 'main'
}

# --- 4) 推送 ----------------------------------------------------------------
Write-Host "`n=== 推送到 GitHub ===" -ForegroundColor Cyan
Write-Host "若远程仓库还不存在，请先到 https://github.com/new 新建空仓库 $Repo（不要初始化 README）" -ForegroundColor Yellow
git push -u origin main
if ($LASTEXITCODE -ne 0) { throw '推送失败。若提示仓库不存在，请先建空仓库；若提示认证失败，请登录 GitHub 后重试。' }

# --- 5) Release（可选）------------------------------------------------------
if (-not $Release) {
    Write-Host "`n完成。要发 Release 请加 -Release 参数。" -ForegroundColor Green
    return
}

$gh = Get-Command gh -ErrorAction SilentlyContinue
if (-not $gh) {
    Write-Warning '未安装 gh CLI，无法自动创建 Release。请安装后重试，或到 GitHub 网页手动上传 out\JongyeolModsI18n.zip。'
    Write-Host '  安装：winget install --id GitHub.cli' -ForegroundColor Yellow
    return
}

if (-not $SkipBuild) {
    Write-Host "`n=== 构建产物 ===" -ForegroundColor Cyan
    & (Join-Path $workspace 'build.ps1')
    if ($LASTEXITCODE -ne 0) { throw '构建失败' }
}

$zip = Join-Path $workspace 'out\JongyeolModsI18n.zip'
if (-not (Test-Path $zip)) { throw "找不到产物：$zip" }

if (-not $Tag) {
    $Tag = 'v1.0.0.0'
}
Write-Host "`n=== 创建 Release $Tag ===" -ForegroundColor Cyan
gh release create $Tag $zip --title $Tag --notes "简体中文汉化独立模组（JALib / JipperResourcePack / BetterCalibration）。安装：把 JongyeolModsI18n.zip 拖进 UMM。"
if ($LASTEXITCODE -ne 0) { throw 'Release 创建失败' }
Write-Host "`n完成：https://github.com/$User/$Repo/releases/tag/$Tag" -ForegroundColor Green
