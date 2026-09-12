# Jongyeol's Mods I18n —— 构建脚本
#
# 把汉化包编译成**一个独立模组**，产物在 out\ 下，并可选打包成 UMM 可直接安装的 zip。
#
# 用法（在本文件所在目录执行）：
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\build.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\build.ps1 -Configuration Debug
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\build.ps1 -NoZip
#
# 说明：只读取游戏目录（编译期引用 JALib.dll / UnityModManager.dll 等）与中文源文件，不写游戏目录。

[CmdletBinding()]
param(
    [string]$Configuration = 'Release',
    [string]$Workspace = $PSScriptRoot,
    [string]$GamePath = 'C:\Program Files (x86)\Steam\steamapps\common\A Dance of Fire and Ice',
    [switch]$NoZip
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

# 必须在使用 ZipArchive* 类型之前加载：PowerShell 在**解析期**就会解析 [类型] 字面量，
# 若把 Add-Type 放在同一脚本后面，解析时类型尚不存在，会直接报 TypeNotFound。
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

if (-not $Workspace) { $Workspace = (Get-Location).Path }
if (-not $GamePath) { $GamePath = 'C:\Program Files (x86)\Steam\steamapps\common\A Dance of Fire and Ice' }

$managed = Join-Path $GamePath 'A Dance of Fire and Ice_Data\Managed'
$mods    = Join-Path $GamePath 'Mods'
$out     = Join-Path $Workspace 'out'

if (-not (Test-Path $managed)) { throw "找不到游戏 Managed 目录：$managed" }
if (-not (Test-Path $mods))    { throw "找不到游戏 Mods 目录：$mods" }

# --- 编译前置：net481 参考程序集（本机未装开发包时用 NuGet 缓存里的）----------------
$refPkg = Join-Path $env:USERPROFILE '.nuget\packages\microsoft.netframework.referenceassemblies.net481'
$refRoot = $null
if (Test-Path $refPkg) {
    $ver = Get-ChildItem $refPkg -Directory | Sort-Object Name -Descending | Select-Object -First 1
    if ($ver) { $refRoot = Join-Path $ver.FullName 'build' }
}
$refProp = if ($refRoot) { "/p:TargetFrameworkRootPath=$refRoot" } else { $null }
if (-not $refRoot) { Write-Warning '未找到 net481 参考程序集包，编译可能失败' }

Write-Host "`n=== 编译 JongyeolModsI18n ($Configuration) ===" -ForegroundColor Cyan
$buildArgs = @('build', (Join-Path $Workspace 'JongyeolModsI18n.csproj'), '-c', $Configuration, '-v', 'minimal', '--nologo')
if ($refProp) { $buildArgs += $refProp }
& dotnet @buildArgs
if ($LASTEXITCODE -ne 0) { throw '编译失败' }

# --- 组装 -------------------------------------------------------------------
$dll = Join-Path $Workspace "bin\$Configuration\net481\JongyeolModsI18n.dll"
if (-not (Test-Path $dll)) { throw "找不到编译产物：$dll" }
if (Test-Path $out) { Remove-Item $out -Recurse -Force }
New-Item -ItemType Directory -Force -Path $out | Out-Null

Copy-Item $dll "$out\JongyeolModsI18n.dll" -Force
# 清单与引导 DLL（已在仓库里维护；引导 DLL 与作者的 JAMod.Bootstrap 一致，直接复用）
foreach ($f in @('Info.json', 'JAModInfo.json', 'JAMod.Bootstrap.dll')) {
    $src = Join-Path $Workspace $f
    if (-not (Test-Path $src)) { $src = Join-Path $mods "JALib\$f" }
    if (-not (Test-Path $src)) { throw "缺少打包必需文件：$f（请放在仓库根目录或 Mods\JALib 下）" }
    Copy-Item $src (Join-Path $out $f) -Force
}

# 汉化表：data\ 是仓库里的权威副本（由 i18n-editor 维护）。
# 1) 同步进 Resources\ —— 供 csproj 作为嵌入资源打进 DLL（联网失败时的兜底 / 保留原文时的英文来源）；
# 2) 复制进 out\localization\ —— 随包发布，用户也能就地查看/修改。
$dataDir = Join-Path $Workspace 'data'
$resSrc = Join-Path $Workspace 'Resources'
if (-not (Test-Path $dataDir)) { throw "找不到汉化表目录：$dataDir（请先运行 i18n-editor fetch）" }
$tables = @(Get-ChildItem $dataDir -Filter '*.ChineseSimplified.json')
if ($tables.Count -eq 0) { throw "汉化表目录为空：$dataDir" }
# 原文兜底表（可能有，也可能没有；没有的模组无法「保留原文」）
$bases = @(Get-ChildItem $dataDir -Filter '*.BaseEnglish.json')
New-Item -ItemType Directory -Force -Path $resSrc | Out-Null
$resDir = Join-Path $out 'localization'
New-Item -ItemType Directory -Force -Path $resDir | Out-Null
foreach ($f in $tables) {
    Copy-Item $f.FullName (Join-Path $resSrc $f.Name) -Force
    Copy-Item $f.FullName $resDir -Force
    Write-Host ("  [汉化表] {0}" -f $f.Name)
}
foreach ($f in $bases) {
    Copy-Item $f.FullName (Join-Path $resSrc $f.Name) -Force
    Copy-Item $f.FullName $resDir -Force
    Write-Host ("  [原文表] {0}" -f $f.Name)
}

# --- 自检 -------------------------------------------------------------------
Write-Host "`n=== 产物自检 ===" -ForegroundColor Cyan
$asm = [System.Reflection.Assembly]::ReflectionOnlyLoadFrom((Join-Path $out 'JongyeolModsI18n.dll'))
$resources = $asm.GetManifestResourceNames()
foreach ($n in $resources) { Write-Host ("  内嵌资源: {0}" -f $n) }
$missingRes = @('JALib.ChineseSimplified.json', 'BetterCalibration.ChineseSimplified.json', 'JipperResourcePack.ChineseSimplified.json') |
    Where-Object { $resources -notcontains $_ }
foreach ($req in @('Info.json', 'JAModInfo.json', 'JAMod.Bootstrap.dll', 'JongyeolModsI18n.dll')) {
    if (-not (Test-Path (Join-Path $out $req))) { throw "产物缺少 $req" }
}

# --- 打包 zip ---------------------------------------------------------------
$zipPath = Join-Path $out 'JongyeolModsI18n.zip'
if (-not $NoZip) {
    if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
    $empty = [System.IO.File]::Open($zipPath, [System.IO.FileMode]::Create); $empty.Close()
    $archive = [System.IO.Compression.ZipFile]::Open($zipPath, [System.IO.Compression.ZipArchiveMode]::Update)
    try {
        Get-ChildItem $out -Recurse -File | Where-Object { $_.Name -ne 'JongyeolModsI18n.zip' } | ForEach-Object {
            # zip 规范要求正斜杠（Windows PowerShell 的 CreateFromDirectory 会写成反斜杠）
            $rel = $_.FullName.Substring($out.Length).TrimStart('\', '/') -replace '\\', '/'
            $entry = $archive.CreateEntry($rel, [System.IO.Compression.CompressionLevel]::Optimal)
            $entry.LastWriteTime = $_.LastWriteTime
            $in = [System.IO.File]::OpenRead($_.FullName)
            try {
                $es = $entry.Open()
                try { $in.CopyTo($es) } finally { $es.Dispose() }
            } finally { $in.Dispose() }
        }
    } finally { $archive.Dispose() }
    Write-Host ("`n  [zip]  {0}  ({1:N0} 字节)" -f 'JongyeolModsI18n.zip', (Get-Item $zipPath).Length) -ForegroundColor Green
}

Write-Host "`n=== 产物清单 ===" -ForegroundColor Green
Get-ChildItem $out -Recurse -File | Sort-Object FullName | ForEach-Object {
    '{0,10:N0}  {1}' -f $_.Length, $_.FullName.Substring($out.Length + 1)
}

if ($missingRes.Count) {
    Write-Warning ("DLL 内缺少内嵌资源：" + ($missingRes -join ', '))
    exit 1
}
Write-Host "`n全部检查通过。" -ForegroundColor Green
Write-Host @"
安装：把 out\JongyeolModsI18n.zip 拖进 UMM，或把 out\ 下的文件复制到 <游戏>\Mods\JongyeolModsI18n\。
生效条件：游戏界面语言为「简体中文」（或对应模组的 CustomLanguage 设为简体中文）。
"@
