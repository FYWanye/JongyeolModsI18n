using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
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
/// Jongyeol's Mods I18n —— 把 Jongyeol 系列模组的简体中文表作为**一个独立模组**分发。
///
/// 工作方式：
/// 1. 用 Harmony 给 <c>JALib.Core.JALocalization.Load</c> 挂 Prefix（劫持各模组的本地化加载）；
/// 2. 当生效语言为简体中文、且总开关与目标模组开关都打开时，
///    Prefix 把内置中文表写进该模组的本地化字段，并返回 false 跳过原方法；
/// 3. 跳过原方法同时阻止了 JALib 从 Google 表格拉取并**写回**
///    <c>localization\&lt;语言&gt;.json</c>（表格没有中文列，云端数据会把中文盖回英文/韩文）；
/// 4. 中文表会顺带落盘到 <c>Mods\&lt;模组&gt;\localization\ChineseSimplified.json</c>，
///    让"直接读该文件"的代码同样拿到中文。
///
/// 所有开关都可在 UMM 的模组设置页里实时切换；关闭后通过调用各模组的
/// <c>JALocalization.Reload()</c> 让原逻辑重新跑一遍，从而恢复原始语言。
///
/// 不使用 JALib 的 JAPatcher：官方版把 <c>JAPatchBaseAttribute.Method</c> 声明为 internal，
/// 外部模组无法在运行时构造补丁；Harmony 是双方共用的底层，跨版本更稳。
/// </summary>
public class Main : JAMod {
    /// <summary>由 JALib 在运行时自动注入。</summary>
    public static Main Instance;

    /// <summary>本模组提供中文的模组 Id，以及对应的设置字段名（未安装的会被自动跳过）。</summary>
    internal static readonly (string Id, string Field)[] TranslatableMods = [
        ("JALib", nameof(ModSettings.EnableJALib)),
        ("BetterCalibration", nameof(ModSettings.EnableBetterCalibration)),
        ("JipperResourcePack", nameof(ModSettings.EnableJipperResourcePack))
    ];

    /// <summary>汉化表的远端地址前缀。改动这里即可换成别的托管位置。</summary>
    private const string RemoteBase =
        "https://raw.githubusercontent.com/FYWanye/JongyeolModsI18n/main/data/";

    /// <summary>远端拉取的超时（毫秒）。失败只影响"更新译文"，不影响游戏。</summary>
    private const int RemoteTimeoutMs = 8000;

    /// <summary>后台线程下载完成的译文（模组Id → JSON），等主线程取用。</summary>
    private static readonly ConcurrentDictionary<string, string> RemoteResults = new();
    /// <summary>远端拉取的失败提示（模组Id → 文案），用于界面显示。</summary>
    private static readonly ConcurrentDictionary<string, string> RemoteErrors = new();
    /// <summary>已经处理过远端结果的模组，避免重复落盘。</summary>
    private static readonly ConcurrentDictionary<string, bool> RemoteApplied = new();

    private Harmony _harmony;
    private readonly Dictionary<string, string> _injected = new();

    /// <summary>用户可编辑的中文表目录（相对本模组目录）：放 <c>&lt;模组Id&gt;.ChineseSimplified.json</c> 即可覆盖内置译文。</summary>
    private string LocalizationDir => System.IO.Path.Combine(Path, "localization");

    /// <summary>由 JALib 自动实例化的设置对象（字段会自动写进 Settings.json）。</summary>
    internal ModSettings ModSetting {
        get {
            try {
                var modSetting = typeof(JAMod).GetField("ModSetting", BindingFlags.Instance | BindingFlags.NonPublic)?.GetValue(this);
                if(modSetting == null) return null;
                // JAMod.Setting => ModSetting.Setting，走 Combine() 分支时 JALib 不会调用 SetupType，
                // 因此这里惰性补一次，把它挂到 Settings.json 的 Setting 节点上（否则设置无法持久化）。
                var settingField = modSetting.GetType().GetField("Setting", BindingFlags.Instance | BindingFlags.NonPublic | BindingFlags.Public);
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

    protected override void OnSetup() {
        try {
            Type localizationType = typeof(JAMod).Assembly.GetType("JALib.Core.JALocalization");
            if(localizationType == null) {
                Error("找不到 JALib.Core.JALocalization，无法注入中文");
                return;
            }
            MethodInfo load = localizationType.GetMethod("Load", BindingFlags.Instance | BindingFlags.NonPublic | BindingFlags.Public);
            MethodInfo prefix = typeof(ModLocalizationPatch).GetMethod(nameof(ModLocalizationPatch.LocalizationLoad), BindingFlags.Static | BindingFlags.NonPublic);
            if(load == null || prefix == null) {
                Error("找不到 JALocalization.Load 或注入方法，无法注入中文");
                return;
            }
            _harmony = new Harmony("JongyeolModsI18n.Localization");
            _harmony.Patch(load, prefix: new HarmonyMethod(prefix));
            Log("已挂载中文本地化注入点（目标 " + TranslatableMods.Length + " 个模组）");
        } catch (Exception e) {
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

    /// <summary>每帧在主线程上处理后台下载结果。</summary>
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

    // ------------------------------------------------------------------ 开关

    /// <summary>总开关。</summary>
    internal bool Enabled => ModSetting?.Enabled ?? true;

    /// <summary>该模组是否被单独关闭了汉化。</summary>
    internal bool IsModEnabled(string modId) {
        ModSettings settings = ModSetting;
        if(settings == null) return true;
        if(modId == "JALib") return settings.EnableJALib;
        if(modId == "BetterCalibration") return settings.EnableBetterCalibration;
        if(modId == "JipperResourcePack") return settings.EnableJipperResourcePack;
        return true;
    }

    // ------------------------------------------------------------------ 注入 / 恢复

    /// <summary>对所有已加载的目标模组重新走一遍本地化加载。</summary>
    internal void ReloadAll() {
        _injected.Clear();
        foreach((string id, _) in TranslatableMods) ReloadIfLoaded(id);
        SaveSetting();
    }

    private void ReloadIfLoaded(string modId) {
        try {
            JAMod mod = JAMod.GetMods(modId);
            if(mod == null) return;
            JALocalization localization = mod.Localization;
            if(localization == null) return;
            // Reload() 会清掉 _curLang 并重走 Load()：
            //  - 开启汉化 → 我们的 Prefix 接管，注入中文；
            //  - 关闭汉化 → Prefix 放行，原逻辑（本地文件 / 云端）自然恢复原始语言。
            MethodInfo reload = localization.GetType().GetMethod("Reload", BindingFlags.Instance | BindingFlags.NonPublic | BindingFlags.Public);
            if(reload != null) {
                reload.Invoke(localization, null);
                return;
            }
            // 旧版 JALib 没有 Reload()：只能直接注入；恢复原始语言需要重启游戏。
            if(Enabled && IsModEnabled(modId)) TryInject(mod, localization);
            else Warning(modId + " 所在 JALib 版本没有 Reload()，关闭汉化需重启游戏才能恢复。");
        } catch (Exception e) {
            LogException("重新加载 " + modId + " 的本地化失败", e);
        }
    }

    /// <summary>直接把内置中文表写进某个模组的本地化字段（供无 Reload() 的旧版兜底）。</summary>
    internal void TryInject(JAMod mod, JALocalization localization) {
        if(localization == null) return;
        if(!TryReadBuiltIn(mod.Name, out string json)) return;
        try {
            if(!ApplyLocalization(localization, ParseTable(json))) {
                Warning("无法把中文表写入 " + mod.Name + " 的本地化字段（已跳过）");
                return;
            }
            EnsureChineseFile(mod, json);
            InvokeLocalizationUpdate(mod);
            _injected[mod.Name] = "已注入";
        } catch (Exception e) {
            LogException("注入 " + mod.Name + " 的中文表失败", e);
        }
    }

    /// <summary>记录一次成功注入（由补丁调用）。</summary>
    internal void MarkInjected(string modId) => _injected[modId] = "已注入";

    /// <summary>该模组当前是否已注入中文（供界面显示）。</summary>
    internal string InjectionState(string modId) {
        if(_injected.TryGetValue(modId, out string text)) return text;
        if(!IsModEnabled(modId)) return "已按设置关闭";
        return "未注入（界面语言不是简体中文？）";
    }

    /// <summary>触发模组的 OnLocalizationUpdate 回调（JAMod 上该入口是 internal，用反射调用）。</summary>
    internal void InvokeLocalizationUpdate(JAMod mod) {
        try {
            typeof(JAMod).GetMethod("OnLocalizationUpdate0", BindingFlags.Instance | BindingFlags.NonPublic)?.Invoke(mod, null);
        } catch {
            // 通知失败不影响功能：界面下一次绘制时会自然读到新表
        }
    }

    internal static Dictionary<string, string> ParseTable(string json) {
        // 部分编辑器另存为 UTF-8 会带 BOM，Newtonsoft 遇 BOM 会直接抛异常，这里先剥掉。
        string data = json;
        if(!string.IsNullOrEmpty(data) && data[0] == '\uFEFF') data = data[1..];
        return JsonConvert.DeserializeObject<Dictionary<string, string>>(data);
    }

    /// <summary>
    /// 把字典写进 <c>JALocalization._localizations</c>。
    ///
    /// 该字段是 <c>System.Collections.Frozen.FrozenDictionary&lt;string,string&gt;</c>，
    /// 由 JALib 自带的 <c>lib\System.Collections.Immutable.dll</c>(v10) 提供，
    /// 而游戏 Managed 下是 v6（不含 Frozen），两者版本冲突，
    /// 因此这里**纯反射**构造，避免在编译期引用任何一个版本。
    ///
    /// 关键点：<c>ToFrozenDictionary</c> 是 <c>FrozenDictionary</c> 静态类上的**扩展方法**，
    /// 在构造泛型类型上按签名查找会返回 null，必须到静态类上按「返回类型 + 参数个数 + 可赋值性」挑选。
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
    /// 在 <c>System.Collections.Frozen.FrozenDictionary</c> 静态类上找到合适的 ToFrozenDictionary：
    /// 返回类型等于目标字段类型，且两个参数都能接收 (Dictionary, EqualityComparer)。
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

    /// <summary>判断参数类型能否接收给定的实参类型（泛型参数按 <paramref name="arguments"/> 代入后判断）。</summary>
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

    /// <summary>
    /// 取某个模组的内置中文表：优先用模组目录下用户可编辑的那份，缺失时回退 DLL 内嵌资源。
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
            // 后台已从 GitHub 下好、且尚未落盘时，先用内存里这份
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

    // ------------------------------------------------------------------ 远端翻译

    /// <summary>在后台线程拉取各模组的最新汉化表；失败只记录提示，不影响任何功能。</summary>
    private void StartRemoteFetch() {
        foreach((string id, _) in TranslatableMods) {
            if(JAMod.GetMods(id) == null) continue;
            string modId = id;
            Task.Run(() => FetchOne(modId));
        }
    }

    /// <summary>后台线程：只做网络下载，绝不碰 Unity / 文件系统写入。</summary>
    private static void FetchOne(string modId) {
        try {
            string url = RemoteBase + modId + ".ChineseSimplified.json";
            using WebClient client = new();
            client.Encoding = System.Text.Encoding.UTF8;
            client.Headers[HttpRequestHeader.UserAgent] = "JongyeolModsI18n";
            client.Headers[HttpRequestHeader.CacheControl] = "no-cache";
            string json = client.DownloadStringTaskAsync(url).GetAwaiter().GetResult();
            if(string.IsNullOrWhiteSpace(json)) {
                RemoteErrors[modId] = "下载失败";
                return;
            }
            RemoteResults[modId] = json;
            RemoteErrors.TryRemove(modId, out _);
        } catch (Exception e) {
            // 静默失败：只留一条给界面看的提示，日志里保留细节
            RemoteErrors[modId] = "下载失败";
            Main.Instance?.Log("拉取 " + modId + " 汉化表失败：" + e.Message);
        }
    }

    /// <summary>主线程：把下载结果落盘并让对应模组立即生效。</summary>
    private void ApplyRemoteResults() {
        foreach((string id, _) in TranslatableMods) {
            if(!RemoteResults.TryGetValue(id, out string json)) continue;
            if(RemoteApplied.ContainsKey(id)) continue;
            JAMod mod = JAMod.GetMods(id);
            if(mod == null) continue;
            RemoteApplied[id] = true;
            try {
                if(RemoteErrors.ContainsKey(id)) continue;
                // 1) 落盘：下次启动即使联网失败也能用最新译文
                string path = System.IO.Path.Combine(mod.Path, "localization", "ChineseSimplified.json");
                System.IO.Directory.CreateDirectory(System.IO.Path.GetDirectoryName(path)!);
                if(!System.IO.File.Exists(path) || System.IO.File.ReadAllText(path) != json)
                    System.IO.File.WriteAllText(path, json);
                // 2) 当前会话立即生效（若已注入过中文，重新加载一次即可）
                if(_injected.ContainsKey(id)) ReloadIfLoaded(id);
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

    /// <summary>把中文表落盘到目标模组目录，保证"读该文件的代码"也拿到中文。</summary>
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

    // ------------------------------------------------------------------ 设置界面

    protected override void OnGUI() {
        ModSettings settings = ModSetting;
        if(settings == null) {
            GUILayout.Label("设置未初始化。");
            return;
        }
        SettingGUI gui = new(this);

        gui.AddSettingToggle(ref settings.Enabled, "启用汉化", ReloadAll);
        GUILayout.Label("<color=grey>总开关。关闭后各模组恢复原始语言（通过重新加载本地化即时生效）。</color>");

        GUILayout.Space(6);
        GUILayout.Label("<b>各模组汉化开关</b>");
        foreach((string id, _) in TranslatableMods) {
            if(JAMod.GetMods(id) == null) {
                GUILayout.Label("<color=grey>  " + id + "：未安装</color>");
                continue;
            }
            if(id == "JALib") gui.AddSettingToggle(ref settings.EnableJALib, "  " + id, ReloadAll);
            else if(id == "BetterCalibration") gui.AddSettingToggle(ref settings.EnableBetterCalibration, "  " + id, ReloadAll);
            else if(id == "JipperResourcePack") gui.AddSettingToggle(ref settings.EnableJipperResourcePack, "  " + id, ReloadAll);
        }

        GUILayout.Space(6);
        GUILayout.Label("<b>当前状态</b>");
        if(!Enabled) {
            GUILayout.Label("<color=grey>  汉化已关闭</color>");
        } else foreach((string id, _) in TranslatableMods) {
            if(JAMod.GetMods(id) == null) continue;
            GUILayout.Label("  " + id + "：" + InjectionState(id) + "（" + TranslationSource(id) + "）");
        }

        GUILayout.Space(6);
        GUILayout.Label("<color=grey>译文优先级：GitHub 最新 → 本地缓存 → DLL 内嵌译文；"
                        + "联网失败会退回后两者，只影响译文新旧，不影响游戏。</color>");
    }
}

/// <summary>
/// 设置字段。JALib 会按 public 字段名自动写进
/// <c>Mods\JongyeolModsI18n\Settings.json</c> 的 <c>Setting</c> 节点。
/// </summary>
internal class ModSettings : JASetting {
    public bool Enabled = true;
    public bool EnableJALib = true;
    public bool EnableBetterCalibration = true;
    public bool EnableJipperResourcePack = true;

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

    private static SystemLanguage GameLanguage() {
        try {
            object value = RdStringLanguage?.GetValue(null);
            if(value is SystemLanguage language) return language;
        } catch {
            // ignored
        }
        return SystemLanguage.English;
    }

    /// <summary>
    /// Prefix：返回 false 表示跳过原 Load（同时也就阻止了云端拉取与写回）。
    /// </summary>
    internal static bool LocalizationLoad(JALocalization __instance) {
        try {
            Main main = Main.Instance;
            if(main == null) return true;
            if(!main.Enabled) return true; // 总开关关闭 → 完全放行，恢复官方行为

            if(!(ModField?.GetValue(__instance) is JAMod mod)) return true;
            if(Array.FindIndex(Main.TranslatableMods, entry => entry.Id == mod.Name) < 0) return true;
            if(!main.IsModEnabled(mod.Name)) return true; // 该模组被单独关闭

            // 本轮生效语言：模组自定语言 > 已缓存的当前语言 > 游戏语言
            SystemLanguage? curLang = (SystemLanguage?) LangField?.GetValue(__instance);
            SystemLanguage? custom = (SystemLanguage?) CustomLanguageProperty?.GetValue(mod);
            SystemLanguage language = custom ?? curLang ?? GameLanguage();

            // 只在简体中文下接管；其它语言交回原逻辑，不影响玩家的语言设置。
            if(language != SystemLanguage.ChineseSimplified) return true;
            // 已经是中文：既不再重复注入，也不再拉云端（这一步就是"汉化不被写回覆盖"的关键）
            if(curLang == SystemLanguage.ChineseSimplified) return false;

            if(!main.TryReadBuiltIn(mod.Name, out string json)) return true;

            // 先落盘再注入，保证磁盘与内存一致
            main.EnsureChineseFile(mod, json);
            if(!Main.ApplyLocalization(__instance, Main.ParseTable(json))) {
                // 注入失败就交回原流程，宁可走官方云端逻辑也不要卡在空表上
                main.Warning("注入 " + mod.Name + " 的中文表失败（已交回官方逻辑）");
                return true;
            }
            LangField?.SetValue(__instance, SystemLanguage.ChineseSimplified);
            main.InvokeLocalizationUpdate(mod);
            main.MarkInjected(mod.Name);
            return false;
        } catch (Exception e) {
            // 任何异常都不能影响原流程
            try {
                Main.Instance?.LogException("注入本地化时出错", e);
            } catch {
                // ignored
            }
            return true;
        }
    }
}
