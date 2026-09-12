# Jongyeol's Mods I18n

把 [Jongye0l](https://github.com/Jongye0l) 的 ADOFAI 模组**简体中文汉化**整合成**一个独立模组**。
装一次即可为多个模组提供中文，并且**被汉化模组更新后中文依然有效**。

## 覆盖范围

| 模组 | 中文条目数 | 上游 |
|---|---|---|
| JALib | 42 | [JALib](https://github.com/Jongye0l/JALib) |
| JipperResourcePack | 169 | [JipperResourcePack](https://github.com/Jongye0l/JipperResourcePack) |
| BetterCalibration | 27 | [BetterCalibration](https://github.com/Jongye0l/BetterCalibration) |

> 只汉化本地化表。**AdofaiTweaks / XPerfect 自带中文，未纳入**。
> 未安装的模组会被自动跳过，不会报错。

## 安装

1. 下载最新 Release 的 `JongyeolModsI18n.zip`
2. **完全退出游戏**
3. 把 zip 拖进 UMM 的「安装模组」窗口
   （或解压后把里面的文件放到 `<游戏>\Mods\JongyeolModsI18n\`）
4. 启动游戏

**生效条件**：游戏界面语言为「简体中文」。
若界面语言不是简体中文，可把各模组 `Settings.json` 里的 `CustomLanguage` 设为 `40`
（`40` = `UnityEngine.SystemLanguage.ChineseSimplified`）。

## 设置（UMM 模组设置页）

| 选项 | 默认 | 说明 |
|---|---|---|
| 启用汉化 | 开 | 总开关。关闭后各模组恢复原始语言 |
| 各模组汉化开关 | 平均开 | `JALib` / `BetterCalibration` / `JipperResourcePack` 各自独立；未安装的显示"未安装" |
| 当前状态 | — | 逐模组显示「已注入 / 已按设置关闭 / 未注入（界面语言不是简体中文？）」 |

开关**实时保存**到 `Mods\JongyeolModsI18n\Settings.json`，并且**不需要重启游戏**——
切换后会调用对应模组的 `JALocalization.Reload()` 重新加载本地化，
开启即注入中文、关闭即恢复原始语言。

## 工作原理

```
游戏启动
 └─ JALib 构造各模组的 JALocalization（此时本模组的 Harmony Prefix 已挂好）
     └─ Prefix 判断：该模组在汉化名单里 && 总开关与模组开关都开 && 生效语言 == 简体中文
         ├─ 读取内置的 <模组Id>.ChineseSimplified.json
         ├─ 落盘到 Mods\<模组>\localization\ChineseSimplified.json
         ├─ 反射写进该模组的本地化字段
         └─ 返回 false —— 跳过原 Load()
```

**为什么跳过原 Load 很关键**：原 `Load()` 会从作者的 Google 表格拉取数据并写回
`localization\<语言>.json`。而表格目前**只有 Korean / English 两列**，
云端数据会把中文盖回英文/韩文。跳过它就等于同时关掉了这条覆盖路径——
这就是"更新后中文不丢"的根本原因。

## 改动译文 / 新增模组

### 改译文（无需重新编译）

在本模组目录建 `localization\`，放入 `<模组Id>.ChineseSimplified.json`：

```
Mods\JongyeolModsI18n\localization\JipperResourcePack.ChineseSimplified.json
```

启动时**优先**读取该文件，找不到才回退 DLL 内嵌译文。

### 改内嵌译文 / 加新模组

1. 编辑或新增 `Resources\<模组Id>.ChineseSimplified.json`（模组 Id 即 `Mods\` 下的文件夹名）
2. 若为**新模组**，在 `Main.TranslatableMods` 数组里加上它，并在设置类里加一个开关字段
3. 重新构建

## 构建

需要：Windows + dotnet SDK + 已安装 ADOFAI 与官方 JALib（编译期引用 `Mods\JALib\JALib.dll`）。

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\build.ps1
# 可选参数：-Configuration Debug、-NoZip
```

产物在 `out\`：`JongyeolModsI18n.zip` 与散件。
脚本会自检内嵌资源与必需文件，并保证 zip 条目为正斜杠。

> ⚠️ **不要去掉 `build.ps1` 的 UTF-8 BOM**：Windows PowerShell 5.1 对无 BOM 的 `.ps1`
> 按 GBK 解码，会把中文注释破坏成语法错误。
>
> ⚠️ 不要把脚本接在 `| Select-Object -First N` 后面：管道提前关闭会让脚本中途被杀，
> `out\` 会少文件。要看输出请 `... *> build.log` 再筛选。

## 实现说明

### 为什么不改 JALib

本模组**不修改 JALib 源码**，对官方版 JALib 直接有效：

- Harmony 是 JALib 与 UMM 共用的底层补丁库，直接用它挂 Prefix，跨版本更稳；
- 官方版把 `JAPatchBaseAttribute.Method` 声明为 `internal`，外部模组无法在运行时构造
  JALib 补丁，所以 Harmony 是唯一可行且更通用的路径；
- 本地化字段 `_localizations` 的类型 `FrozenDictionary<string,string>` 由 JALib 自带的
  `lib\System.Collections.Immutable.dll`(v10) 提供，与游戏 `Managed` 下的 v6 冲突，
  因此本模组**纯反射**构造，不在编译期引用任何一个版本。

### 已知限制

- **只翻译本地化表**：被汉化模组里**硬编码**的界面字面量（例如 JALib 模组列表里的
  `[Updating...]` 状态）不在覆盖范围内，需要改那些模组的 DLL 本身才能汉化。
- **依赖游戏语言**：只有界面语言是简体中文（或该模组 `CustomLanguage` 设为中文）时才接管。
  这是有意设计，避免影响其它语言玩家。
- **`FrozenDictionary` 的反射构造无法在 .NET Framework（如 PowerShell）下验证**，
  因为该类型只在游戏所用的 Mono 运行时可解析。代码已写成防御式 + 多路径回退，
  失败时交回官方逻辑并打日志。

## 排查

| 现象 | 原因与处理 |
|---|---|
| 界面还是英文 | 确认游戏界面语言是「简体中文」；或把对应模组 `Settings.json` 的 `CustomLanguage` 设为 `40`。装好先重启一次游戏 |
| 只有部分模组是中文 | 到本模组设置页看「各模组汉化开关」是否被关掉，以及「当前状态」显示什么 |
| 中文过一会儿变回英文 | 说明前缀没生效，仍被云端写回。检查 UMM 日志有没有报错，并确认本模组已启用 |
| 装完 UMM 里看不到本模组 | zip 里必须含 `Info.json`；用 `build.ps1` 重新打包可确保齐全 |
| 切换开关没反应 | 该模组的 JALib 版本缺少 `JALocalization.Reload()`，需重启游戏生效（日志会提示） |
| `build.ps1` 报 `Missing expression after ','` 之类 | 脚本的 UTF-8 BOM 被编辑器去掉了，用编辑器另存为「UTF-8 带 BOM」 |
| 构建后 `out\` 少文件 | 不要把脚本接在 `\| Select-Object -First N` 后面（管道提前关闭会中断脚本） |

## 致谢与许可

- 模组本体与本地化数据版权归 [Jongye0l](https://github.com/Jongye0l) 所有；
  本项目仅提供简体中文译文与注入实现，沿用上游的 **BSD 3-Clause** 许可（见 [LICENSE](LICENSE)）。
- 感谢 [UnityModManager](https://www.nexusmods.com/site/mods/21) 与 [Harmony](https://github.com/pardeike/Harmony)。

## 仓库结构

```
JongyeolModsI18n\
├─ Info.json                 UMM 清单（Id / 入口 / LoadAfter）
├─ JAModInfo.json            JALib 清单（程序集 / 类名）
├─ JAMod.Bootstrap.dll       引导 DLL（与作者的 JAMod.Bootstrap 一致）
├─ JongyeolModsI18n.csproj   工程文件
├─ Main.cs                   模组主体：Harmony Prefix + 设置面板
├─ Resources\                内嵌中文表
├─ build.ps1                 构建脚本
├─ Directory.Build.props     编译配置（GameManagedPath）
└─ out\                      构建产物（git 忽略）
```
