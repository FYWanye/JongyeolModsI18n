using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Linq;
using System.Net;
using System.Reflection;
using System.Threading.Tasks;
using HarmonyLib;
using JALib.Core;
using JALib.Core.Setting;
using JALib.Tools;
using Newtonsoft.Json;
using UnityEngine;

namespace JongyeolModsI18n;

/// <summary>
/// 把 Jongyeol 系列模组的简体中文表作为一个独立模组分发。
///
/// 原理：
/// 1. 用 Harmony 给 <c>JALib.Core.JALocalization.Load</c> 挂 Prefix，劫持各模组的本地化加载；
/// 2. 当总开关与目标模组开关都打开（默认也不要求界面语言为中文）时，
///    前缀把内置中文表写进该模组的本地化字段，并返回 false 跳过原方法；
/// 3. 跳过原方法同时阻止了 JALib 从 Google 表格拉取并写回
///    <c>localization\&lt;语言&gt;.json</c>——表格只有英文/韩文列，云端会把中文盖回去；
/// 4. 中文表会顺带落盘到 <c>Mods\&lt;模组&gt;\localization\ChineseSimplified.json</c>，
///    让「直接读该文件」的代码同样拿到中文。
///
/// 不使用 JALib 的 JAPatcher：官方把 <c>JAPatchBaseAttribute.Method</c> 声明为 internal，
/// 外部模组无法在运行时构造补丁；Harmony 是双方共用的底层，跨版本更稳。
/// </summary>
public class Main : JAMod {
    /// <summary>由 JALib 在运行时自动注入的单例。</summary>
    public static Main Instance;

    /// <summary>本模组提供中文的模组 Id（未安装的会被自动跳过）。</summary>
    internal static readonly string[] TranslatableMods = ["JALib", "BetterCalibration", "JipperResourcePack"];

    /// <summary>汉化表远端地址前缀（GitHub raw）。改这里即可换托管位置。</summary>
    private const string RemoteBase = "https://raw.githubusercontent.com/FYWanye/JongyeolModsI18n/main/data/";

    /// <summary>远端拉取超时（毫秒）。超时只影响「更新译文」，不影响游戏。</summary>
    private const int RemoteTimeoutMs = 8000;

    /// <summary>后台线程下载完成的译文（模组Id → JSON），等主线程取用。</summary>
    private static readonly ConcurrentDictionary<string, string> RemoteResults = new();
    /// <summary>远端拉取的失败提示（模组Id → 文案），用于界面显示。</summary>
    private static readonly ConcurrentDictionary<string, string> RemoteErrors = new();
    /// <summary>已处理过远端结果的模组，避免重复落盘。</summary>
    private static readonly ConcurrentDictionary<string, bool> RemoteApplied = new();
    /// <summary>原文兜底表缓存（模组Id → 键到英文原文）。</summary>
    private static readonly ConcurrentDictionary<string, Dictionary<string, string>> BaseCache = new();

    private Harmony _harmony;
    /// <summary>补丁是否已成功挂载（用于区分「补丁没挂上」与「挂上了但没被调用」）。</summary>
    private static bool _patchMounted;

    /// <summary>各模组的注入状态（模组Id → 状态文案）。设置页「当前状态」的唯一数据源。</summary>
    private readonly ConcurrentDictionary<string, string> _states = new();

    /// <summary>用户可编辑的中文表目录（相对本模组目录）：放 &lt;模组Id&gt;.ChineseSimplified.json 即可覆盖内置译文。</summary>
    private string LocalizationDir => System.IO.Path.Combine(Path, "localization");

    /// <summary>
    /// 由 JALib 自动实例化的设置对象（字段会自动写进 Settings.json）。
    /// JAMod.ModSetting 与 JAModSetting.Setting 都是 internal，走反射；
    /// 走 Combine() 分支时 JALib 不会调 SetupType，这里惰性补一次，否则设置无法持久化。
    /// </summary>
    internal ModSettings ModSetting {
        get {
            try {
                object modSetting = typeof(JAMod).GetField("ModSetting", BindingFlags.Instance | BindingFlags.NonPublic)?.GetValue(this);
                if(modSetting == null) return null;
                FieldInfo settingField = modSetting.GetType().GetField("Setting", BindingFlags.Instance | BindingFlags.NonPublic | BindingFlags.Public);
                if(settingField?.GetValue(modSetting) == null) {
                    MethodInfo setup = modSetting.GetType().GetMethod("SetupType", BindingFlags.Instance | BindingFlags.NonPublic | BindingFlags.Public);
                    setup?.Invoke(modSetting, [typeof(ModSettings), this]);
                }
                return settingField?.GetValue(modSetting) as ModSettings;
            } catch (Exception e) {
                Error("初始化设置失败：" + e.Message);
                return null;
            }
        }
    }

    // ---------------------------------------------------------------- 生命周期

    protected override void OnSetup() {
        try {
            Type localizationType = typeof(JAMod).Assembly.GetType("JALib.Core.JALocalization");
            MethodInfo load = localizationType?.GetMethod("Load", BindingFlags.Instance | BindingFlags.NonPublic | BindingFlags.Public);
            MethodInfo prefix = typeof(ModLocalizationPatch).GetMethod(nameof(ModLocalizationPatch.LocalizationLoad), BindingFlags.Static | BindingFlags.NonPublic);
            if(load == null || prefix == null) {
                Error("找不到 JALocalization.Load 或注入方法，无法注入中文");
                return;
            }
            _harmony = new Harmony("JongyeolModsI18n.Localization");
            _harmony.Patch(load, prefix: new HarmonyMethod(prefix));
            _patchMounted = true;
            Log("已挂载中文本地化注入点（目标 " + TranslatableMods.Length + " 个模组）");
        } catch (Exception e) {
            _patchMounted = false;
            LogException("挂载本地化注入补丁失败", e);
        }
    }

    protected override void OnEnable() {
        try {
            System.IO.Directory.CreateDirectory(LocalizationDir);
        } catch (Exception e) {
            LogException("创建 localization 目录失败", e);
        }
        ReloadAll();
        // 后台拉取最新汉化表：不阻塞主线程，失败也只记一条提示。
        StartRemoteFetch();
    }

    /// <summary>每帧在主线程处理后台下载结果。</summary>
    protected override void OnUpdate(float deltaTime) {
        ApplyRemoteResults();
    }

    protected override void OnUnload() {
        try {
            _harmony?.UnpatchAll("JongyeolModsI18n.Localization");
        } catch {
            // ignored
        }
        _harmony = null;
    }

    // ---------------------------------------------------------------- 开关

    /// <summary>「启用汉化」总开关（区别于 JAMod.Enabled 的模组启用状态）。</summary>
    internal bool MasterEnabled => ModSetting?.Enabled ?? true;

    /// <summary>某个模组的汉化开关是否打开。</summary>
    internal bool IsModEnabled(string modId) {
        ModSettings settings = ModSetting;
        if(settings == null) return true;
        return modId switch {
            "JALib" => settings.EnableJALib,
            "BetterCalibration" => settings.EnableBetterCalibration,
            "JipperResourcePack" => settings.EnableJipperResourcePack,
            _ => true
        };
    }

    /// <summary>该 Id 是否在汉化名单里。</summary>
    internal static bool IsTranslatable(string modId) => Array.IndexOf(TranslatableMods, modId) >= 0;

    // ---------------------------------------------------------------- 注入 / 状态

    /// <summary>清空状态缓存，对所有已加载的目标模组重新注入。</summary>
    internal void ReloadAll() {
        _states.Clear();
        foreach(string id in TranslatableMods) ReloadIfLoaded(id);
        SaveSetting();
    }

    private void ReloadIfLoaded(string modId) {
        try {
            JAMod mod = JAMod.GetMods(modId);
            if(mod == null) return;
            JALocalization localization = mod.Localization;
            if(localization == null) return;
            if(!MasterEnabled || !IsModEnabled(modId)) {
                // 关闭状态：不注入；恢复原始语言需重启游戏（JALib 无 Reload 接口）。
                Warning(modId + "：汉化已关闭，需重启游戏才能恢复原始语言");
                return;
            }
            if(InjectInto(localization, mod)) InvokeLocalizationUpdate(mod);
        } catch (Exception e) {
            LogException("重新加载 " + modId + " 的本地化失败", e);
        }
    }

    /// <summary>
    /// 把某模组的中文表注入它的本地化字段；成功返回 true，并把状态记为「已注入」。
    /// 每次都重新注入：JALib 可能已用本地文件或云端填充过该字段，跳过会漏写中文。
    /// </summary>
    internal bool InjectInto(JALocalization localization, JAMod mod) {
        if(localization == null || mod == null) return false;
        string modId = mod.Name;
        if(!TryReadBuiltIn(modId, out string json)) {
            Warning("没有 " + modId + " 的中文表（内嵌与本地缓存都缺失）");
            MarkState(modId, "注入失败：没有中文表");
            return false;
        }
        try {
            // 先落盘再注入，保证磁盘与内存一致。
            EnsureChineseFile(mod, json);
            Dictionary<string, string> table = ApplyKeepOriginal(modId, ParseTable(json), KeepTerms());
            if(table.Count == 0) {
                Warning(modId + " 的中文表是空的");
                MarkState(modId, "注入失败：中文表为空");
                return false;
            }
            if(!ApplyLocalization(localization, table)) {
                Warning("无法把中文表写入 " + modId + " 的本地化字段");
                MarkState(modId, "注入失败：无法写入本地化字段");
                return false;
            }
            MarkState(modId, "已注入");
            return true;
        } catch (Exception e) {
            LogException("注入 " + modId + " 的中文表失败", e);
            MarkState(modId, "注入失败（看日志）");
            return false;
        }
    }

    /// <summary>记录一个模组的显示状态。</summary>
    private void MarkState(string modId, string state) => _states[modId] = state;

    /// <summary>该模组当前状态（供设置页显示）。</summary>
    internal string InjectionState(string modId) {
        if(!MasterEnabled) return "总开关已关闭";
        if(_states.TryGetValue(modId, out string state)) return state;
        if(!IsModEnabled(modId)) return "已按设置关闭";
        if(!_patchMounted) return "未接管（补丁没挂上，请把日志发给作者）";
        return "未接管（该模组尚未加载本地化）";
    }

    /// <summary>触发模组的 OnLocalizationUpdate 回调（JAMod 上该入口是 internal，走反射）。</summary>
    internal void InvokeLocalizationUpdate(JAMod mod) {
        try {
            typeof(JAMod).GetMethod("OnLocalizationUpdate0", BindingFlags.Instance | BindingFlags.NonPublic)?.Invoke(mod, null);
        } catch {
            // 通知失败不影响功能：界面下一次绘制时会自然读到新表。
        }
    }

    // ---------------------------------------------------------------- 反射工具

    internal static Dictionary<string, string> ParseTable(string json) {
        // 部分编辑器另存为 UTF-8 会带 BOM，Newtonsoft 遇 BOM 会抛异常，先剥掉。
        string data = json;
        if(!string.IsNullOrEmpty(data) && data[0] == '\uFEFF') data = data[1..];
        return JsonConvert.DeserializeObject<Dictionary<string, string>>(data);
    }

    /// <summary>
    /// 把字典写进 <c>JALocalization._localizations</c>。
    /// 该字段是 <c>FrozenDictionary&lt;string,string&gt;</c>，由 JALib 自带的
    /// <c>System.Collections.Immutable.dll</c>(v10) 提供，与游戏 Managed 下的 v6 冲突，
    /// 因此纯反射构造，避免编译期引用任一版本。
    /// </summary>
    internal static bool ApplyLocalization(object localization, Dictionary<string, string> table) {
        FieldInfo field = localization.GetType().GetField("_localizations", BindingFlags.Instance | BindingFlags.NonPublic);
        if(field == null) return false;
        Type frozenType = field.FieldType;
        if(frozenType.IsInstanceOfType(table)) {
            field.SetValue(localization, table);
            return true;
        }
        Type[] arguments = frozenType.IsGenericType ? frozenType.GetGenericArguments() : [typeof(string), typeof(string)];
        MethodInfo toFrozen = FindToFrozenDictionary(frozenType, arguments, table.GetType());
        if(toFrozen == null) return false;
        MethodInfo closed = toFrozen.IsGenericMethodDefinition ? toFrozen.MakeGenericMethod(arguments) : toFrozen;
        field.SetValue(localization, closed.Invoke(null, [table, EqualityComparer<string>.Default]));
        return true;
    }

    /// <summary>
    /// 在 <c>FrozenDictionary</c> 静态类上找 ToFrozenDictionary：
    /// 返回类型等于目标字段类型，且两个参数分别能接收 (Dictionary, EqualityComparer)。
    /// ToFrozenDictionary 是扩展方法，在构造泛型类型上按签名查会返回 null，必须到静态类上挑。
    /// </summary>
    private static MethodInfo FindToFrozenDictionary(Type frozenType, Type[] arguments, Type sourceType) {
        Type staticClass = frozenType.Assembly.GetType("System.Collections.Frozen.FrozenDictionary");
        if(staticClass == null) return null;
        foreach(MethodInfo method in staticClass.GetMethods(BindingFlags.Public | BindingFlags.Static)) {
            if(method.Name != "ToFrozenDictionary") continue;
            ParameterInfo[] parameters = method.GetParameters();
            if(parameters.Length != 2) continue;
            if(!IsClosedGenericOf(method.ReturnType, frozenType, arguments)) continue;
            if(!IsMatchable(parameters[0].ParameterType, sourceType, arguments)) continue;
            if(!IsMatchable(parameters[1].ParameterType, typeof(EqualityComparer<string>), arguments)) continue;
            return method;
        }
        return null;
    }

    /// <summary>判断 <paramref name="candidate"/> 在把泛型参数替换为 <paramref name="arguments"/> 后是否等于 <paramref name="target"/>。</summary>
    private static bool IsClosedGenericOf(Type candidate, Type target, Type[] arguments) {
        try {
            if(candidate == target) return true;
            if(!candidate.IsGenericType) return false;
            Type definition = candidate.GetGenericTypeDefinition();
            Type closed = arguments.Length == 2 ? definition.MakeGenericType(arguments[0], arguments[1]) : definition.MakeGenericType(arguments);
            return closed == target;
        } catch {
            return false;
        }
    }

    /// <summary>判断参数类型能否接收给定实参类型（泛型参数按 <paramref name="arguments"/> 代入后判断）。</summary>
    private static bool IsMatchable(Type parameterType, Type argumentType, Type[] arguments) {
        try {
            if(parameterType.ContainsGenericParameters) {
                if(!parameterType.IsGenericType) return false;
                parameterType = parameterType.GetGenericTypeDefinition().MakeGenericType(arguments);
            }
            return parameterType.IsAssignableFrom(argumentType);
        } catch {
            return false;
        }
    }

    // ---------------------------------------------------------------- 译文读取

    /// <summary>
    /// 取某模组的内置中文表。优先级：本模组目录下用户可编辑的文件 → 本次下载的 → DLL 内嵌资源。
    /// </summary>
    internal bool TryReadBuiltIn(string modId, out string json) {
        json = null;
        if(string.IsNullOrEmpty(modId)) return false;
        string key = modId.Replace('.', '_') + ".ChineseSimplified.json";
        try {
            string userPath = System.IO.Path.Combine(LocalizationDir, key);
            if(System.IO.File.Exists(userPath)) {
                json = System.IO.File.ReadAllText(userPath);
                return true;
            }
            if(RemoteResults.TryGetValue(modId, out string remote) && !string.IsNullOrEmpty(remote)) {
                json = remote;
                return true;
            }
            using System.IO.Stream stream = typeof(Main).Assembly.GetManifestResourceStream(key);
            if(stream == null) return false;
            using System.IO.StreamReader reader = new(stream, System.Text.Encoding.UTF8);
            json = reader.ReadToEnd();
            return true;
        } catch (Exception e) {
            LogException("读取 " + modId + " 的中文表失败", e);
            return false;
        }
    }

    // ---------------------------------------------------------------- 远端翻译

    /// <summary>为每个已安装的目标模组在后台线程拉取最新汉化表。</summary>
    private void StartRemoteFetch() {
        foreach(string id in TranslatableMods) {
            if(JAMod.GetMods(id) == null) continue;
            string modId = id;
            Task.Run(() => FetchOne(modId));
        }
    }

    /// <summary>后台线程：只做网络下载，绝不碰 Unity / 文件系统写入。</summary>
    private static void FetchOne(string modId) {
        try {
            string url = RemoteBase + modId + ".ChineseSimplified.json";
            string json;
            using(WebClient client = new()) {
                client.Encoding = System.Text.Encoding.UTF8;
                client.Headers[HttpRequestHeader.UserAgent] = "JongyeolModsI18n";
                client.Headers[HttpRequestHeader.CacheControl] = "no-cache";
                Task<string> download = client.DownloadStringTaskAsync(url);
                if(!download.Wait(RemoteTimeoutMs)) {
                    client.CancelAsync();
                    RemoteErrors[modId] = "下载超时";
                    return;
                }
                json = download.Result;
            }
            if(string.IsNullOrWhiteSpace(json)) {
                RemoteErrors[modId] = "下载失败";
                return;
            }
            RemoteResults[modId] = json;
            RemoteErrors.TryRemove(modId, out _);
        } catch (Exception e) {
            // 静默失败：只留一条给界面看的提示，日志里保留细节。
            RemoteErrors[modId] = "下载失败";
            Main.Instance?.Log("拉取 " + modId + " 汉化表失败：" + e.Message);
        }
    }

    /// <summary>主线程：把下载结果落盘，并让对应模组立即生效。</summary>
    private void ApplyRemoteResults() {
        foreach(string id in TranslatableMods) {
            if(!RemoteResults.TryGetValue(id, out string json)) continue;
            if(RemoteApplied.ContainsKey(id)) continue;
            JAMod mod = JAMod.GetMods(id);
            if(mod == null) continue;
            RemoteApplied[id] = true;
            if(RemoteErrors.ContainsKey(id)) continue;
            try {
                // 1) 落盘：下次启动即使联网失败也能用最新译文。
                string path = System.IO.Path.Combine(mod.Path, "localization", "ChineseSimplified.json");
                System.IO.Directory.CreateDirectory(System.IO.Path.GetDirectoryName(path)!);
                if(!System.IO.File.Exists(path) || System.IO.File.ReadAllText(path) != json)
                    System.IO.File.WriteAllText(path, json);
                // 2) 当前会话立即生效（若已注入过中文，重新注入一次即可）。
                if(_states.TryGetValue(id, out string state) && state == "已注入") ReloadIfLoaded(id);
                Log("已从 GitHub 更新 " + id + " 的汉化表");
            } catch (Exception e) {
                Log("应用 " + id + " 的远端汉化表失败：" + e.Message);
            }
        }
    }

    /// <summary>该模组的译文来源，用于界面显示。</summary>
    internal string TranslationSource(string modId) {
        if(RemoteApplied.ContainsKey(modId) && !RemoteErrors.ContainsKey(modId)) return "已从 GitHub 更新";
        if(RemoteErrors.TryGetValue(modId, out string reason)) return "使用内置译文（" + reason + "）";
        return "使用内置译文";
    }

    /// <summary>把中文表落盘到目标模组目录，保证「读该文件的代码」也拿到中文。</summary>
    internal void EnsureChineseFile(JAMod mod, string json) {
        try {
            string dir = System.IO.Path.Combine(mod.Path, "localization");
            System.IO.Directory.CreateDirectory(dir);
            string path = System.IO.Path.Combine(dir, "ChineseSimplified.json");
            // 内容一致就不重写，避免每次启动都刷新文件时间戳。
            if(System.IO.File.Exists(path) && System.IO.File.ReadAllText(path) == json) return;
            System.IO.File.WriteAllText(path, json);
        } catch (Exception e) {
            LogException("写入 " + mod.Name + " 的 localization/ChineseSimplified.json 失败", e);
        }
    }

    // ---------------------------------------------------------------- 保留原文

    /// <summary>把设置里的词列表拆成小写数组（支持中英文逗号、分号、空格、换行分隔）。</summary>
    internal string[] KeepTerms() {
        string raw = ModSetting?.KeepOriginal;
        if(string.IsNullOrWhiteSpace(raw)) return [];
        return raw
            .Split([',', '，', ';', '；', '\n', '\r', ' ', '\t'], StringSplitOptions.RemoveEmptyEntries)
            .Select(term => term.Trim().ToLowerInvariant())
            .Where(term => term.Length > 0)
            .Distinct()
            .ToArray();
    }

    /// <summary>该键是否命中「保留原文」规则（键名包含所列词即命中）。</summary>
    internal static bool ShouldKeepOriginal(string key, string[] terms) {
        if(terms.Length == 0) return false;
        string lower = key.ToLowerInvariant();
        foreach(string term in terms) {
            if(lower.Contains(term)) return true;
        }
        return false;
    }

    /// <summary>
    /// 取某模组的原文兜底表（&lt;模组Id&gt;.BaseEnglish.json），供「保留原文」把中文换回英文。
    /// 用户可在本模组 localization 目录放同名文件覆盖。
    /// </summary>
    internal static Dictionary<string, string> BaseTable(string modId) {
        if(BaseCache.TryGetValue(modId, out Dictionary<string, string> cached)) return cached;
        Dictionary<string, string> table = [];
        string key = modId.Replace('.', '_') + ".BaseEnglish.json";
        try {
            string userPath = System.IO.Path.Combine(Main.Instance?.LocalizationDir ?? "", key);
            if(!string.IsNullOrEmpty(Main.Instance?.LocalizationDir) && System.IO.File.Exists(userPath)) {
                table = JsonConvert.DeserializeObject<Dictionary<string, string>>(System.IO.File.ReadAllText(userPath)) ?? [];
            } else {
                using System.IO.Stream stream = typeof(Main).Assembly.GetManifestResourceStream(key);
                if(stream != null) {
                    using System.IO.StreamReader reader = new(stream, System.Text.Encoding.UTF8);
                    table = JsonConvert.DeserializeObject<Dictionary<string, string>>(reader.ReadToEnd()) ?? [];
                }
            }
        } catch (Exception e) {
            Main.Instance?.Log("读取 " + modId + " 的原文兜底表失败：" + e.Message);
        }
        BaseCache[modId] = table;
        return table;
    }

    /// <summary>
    /// 按「保留原文」规则生成最终要注入的表：命中的键换回英文原文。
    /// 没有原文的键（属本模组新增）保持中文。
    /// </summary>
    internal static Dictionary<string, string> ApplyKeepOriginal(string modId, Dictionary<string, string> table, string[] terms) {
        if(terms.Length == 0) return table;
        Dictionary<string, string> baseTable = BaseTable(modId);
        if(baseTable.Count == 0) return table;
        foreach(string key in table.Keys.ToList()) {
            if(!ShouldKeepOriginal(key, terms)) continue;
            if(baseTable.TryGetValue(key, out string original) && !string.IsNullOrEmpty(original))
                table[key] = original;
        }
        return table;
    }

    /// <summary>统计某模组里命中「保留原文」的条目数，以及其中没有原文可用的条数。</summary>
    internal bool KeepOriginalCount(string modId, string[] terms, out int hit, out int noBase) {
        hit = 0;
        noBase = 0;
        if(!TryReadBuiltIn(modId, out string json)) return false;
        Dictionary<string, string> table = ParseTable(json);
        Dictionary<string, string> baseTable = BaseTable(modId);
        foreach(string key in table.Keys) {
            if(!ShouldKeepOriginal(key, terms)) continue;
            hit++;
            if(!baseTable.TryGetValue(key, out string original) || string.IsNullOrEmpty(original)) noBase++;
        }
        return true;
    }

    // ---------------------------------------------------------------- 设置界面

    protected override void OnGUI() {
        ModSettings settings = ModSetting;
        if(settings == null) {
            GUILayout.Label("设置未初始化。");
            return;
        }
        SettingGUI gui = new(this);

        gui.AddSettingToggle(ref settings.Enabled, "启用汉化", ReloadAll);
        GUILayout.Label("<color=grey>总开关。关闭后各模组恢复原始语言（重新加载本地化即时生效）。</color>");

        GUILayout.Space(6);
        gui.AddSettingToggle(ref settings.AlwaysChinese, "始终使用中文（不判断界面语言）", ReloadAll);
        GUILayout.Label("<color=grey>默认开启：装本模组就是要中文，因此不要求把游戏界面语言切成简体中文。"
                        + "关闭后仅在语言为简体中文时生效。</color>");

        GUILayout.Space(6);
        GUILayout.Label("<b>各模组汉化开关</b>");
        foreach(string id in TranslatableMods) {
            if(JAMod.GetMods(id) == null) {
                GUILayout.Label("<color=grey>  " + id + "：未安装</color>");
                continue;
            }
            DrawModToggle(gui, settings, id);
        }

        GUILayout.Space(6);
        GUILayout.Label("<b>保留原文的词</b>");
        GUILayout.Label("<color=grey>不想被翻译的词，用逗号或空格分隔（不区分大小写，按条目名匹配，包含即命中）。"
                        + "例：combo, best, attempt</color>");
        string keep = settings.KeepOriginal ?? "";
        string edited = GUILayout.TextField(keep, GUILayout.ExpandWidth(true));
        if(edited != keep) {
            settings.KeepOriginal = edited;
            SaveSetting();
            ReloadAll();
        }
        string[] terms = KeepTerms();
        if(terms.Length > 0) {
            GUILayout.Label("<color=grey>  当前生效 " + terms.Length + " 个词：" + string.Join("、", terms) + "</color>");
            foreach(string id in TranslatableMods) {
                if(JAMod.GetMods(id) == null) continue;
                if(KeepOriginalCount(id, terms, out int hit, out int noBase))
                    GUILayout.Label("  " + id + "：命中 " + hit + " 条" + (noBase > 0 ? "（其中 " + noBase + " 条没有原文，仍显示中文）" : ""));
            }
        }

        GUILayout.Space(6);
        GUILayout.Label("<b>当前状态</b>");
        if(!MasterEnabled) {
            GUILayout.Label("<color=grey>  汉化已关闭</color>");
        } else foreach(string id in TranslatableMods) {
            if(JAMod.GetMods(id) == null) continue;
            GUILayout.Label("  " + id + "：" + InjectionState(id) + "（" + TranslationSource(id) + "）");
        }

        GUILayout.Space(6);
        GUILayout.Label("<color=grey>译文优先级：GitHub 最新 → 本地缓存 → DLL 内嵌译文；"
                        + "联网失败会退回后两者，只影响译文新旧，不影响游戏。</color>");
    }

    /// <summary>按模组 Id 画出一个汉化开关（需要 ref 到具体设置字段，故用 switch 逐个映射）。</summary>
    private void DrawModToggle(SettingGUI gui, ModSettings settings, string id) {
        switch(id) {
            case "JALib":
                gui.AddSettingToggle(ref settings.EnableJALib, "  " + id, ReloadAll);
                break;
            case "BetterCalibration":
                gui.AddSettingToggle(ref settings.EnableBetterCalibration, "  " + id, ReloadAll);
                break;
            case "JipperResourcePack":
                gui.AddSettingToggle(ref settings.EnableJipperResourcePack, "  " + id, ReloadAll);
                break;
        }
    }
}

/// <summary>
/// 设置字段。JALib 会按 public 字段名自动写进
/// <c>Mods\JongyeolModsI18n\Settings.json</c> 的 <c>Setting</c> 节点。
/// </summary>
internal class ModSettings : JASetting {
    public bool Enabled = true;
    /// <summary>不判断界面语言，一律使用中文。</summary>
    public bool AlwaysChinese = true;
    public bool EnableJALib = true;
    public bool EnableBetterCalibration = true;
    public bool EnableJipperResourcePack = true;
    /// <summary>希望保留英文原文的词（逗号/换行分隔，匹配键名，不区分大小写）。</summary>
    public string KeepOriginal = "";

    internal ModSettings(JAMod mod, Newtonsoft.Json.Linq.JObject jsonObject = null) : base(mod, jsonObject) {
    }
}

/// <summary>挂到 <c>JALib.Core.JALocalization.Load</c> 上的补丁。</summary>
internal static class ModLocalizationPatch {
    private static readonly FieldInfo ModField = typeof(JALocalization).GetField("_jaMod", BindingFlags.Instance | BindingFlags.NonPublic);
    private static readonly FieldInfo LangField = typeof(JALocalization).GetField("_curLang", BindingFlags.Instance | BindingFlags.NonPublic);
    /// <summary>JAMod.CustomLanguage 是 protected internal，外部程序集不可直接读，走反射。</summary>
    private static readonly PropertyInfo CustomLanguageProperty = typeof(JAMod).GetProperty("CustomLanguage", BindingFlags.Instance | BindingFlags.NonPublic | BindingFlags.Public);
    /// <summary>游戏当前语言。RDString 在 Assembly-CSharp（游戏本体）里，为避免引入该引用，用反射读取。</summary>
    private static readonly PropertyInfo RdStringLanguage = Type.GetType("RDString, Assembly-CSharp")?.GetProperty("language", BindingFlags.Public | BindingFlags.Static);

    /// <summary>Prefix：返回 false 表示跳过原 Load（同时阻止云端拉取与写回）。</summary>
    internal static bool LocalizationLoad(JALocalization __instance) {
        try {
            return Prefix(__instance);
        } catch (Exception e) {
            // 任何异常都不能影响原流程。
            try {
                Main.Instance?.LogException("注入本地化时出错", e);
            } catch {
                // ignored
            }
            return true;
        }
    }

    private static bool Prefix(JALocalization __instance) {
        Main main = Main.Instance;
        if(main == null) return true;
        if(!main.MasterEnabled) return true; // 总开关关闭 → 完全放行，恢复官方行为
        if(!(ModField?.GetValue(__instance) is JAMod mod)) return true;
        if(!Main.IsTranslatable(mod.Name)) return true;
        if(!main.IsModEnabled(mod.Name)) return true; // 该模组被单独关闭
        if(!UsesChinese(main, mod, __instance)) return true;

        if(!main.InjectInto(__instance, mod)) return true;

        LangField?.SetValue(__instance, SystemLanguage.ChineseSimplified);
        main.InvokeLocalizationUpdate(mod);
        return false;
    }

    /// <summary>
    /// 判断是否应接管该模组。默认「始终使用中文」无条件接管；
    /// 只有用户主动关掉该选项时，才按语言设置决定是否接管。
    /// </summary>
    private static bool UsesChinese(Main main, JAMod mod, JALocalization __instance) {
        bool alwaysChinese = main.ModSetting?.AlwaysChinese ?? true;
        if(alwaysChinese) return true;
        SystemLanguage? custom = (SystemLanguage?) CustomLanguageProperty?.GetValue(mod);
        SystemLanguage language = custom ?? (SystemLanguage?) LangField?.GetValue(__instance) ?? GameLanguage();
        return language == SystemLanguage.ChineseSimplified;
    }

    private static SystemLanguage GameLanguage() {
        try {
            if(RdStringLanguage?.GetValue(null) is SystemLanguage language) return language;
        } catch {
            // ignored
        }
        return SystemLanguage.English;
    }
}
