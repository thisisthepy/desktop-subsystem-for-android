using System.Text.Json;
using System.Text.RegularExpressions;

namespace Dsa.Settings.Core;

/// <summary>Asks the DSA service, through the dsa command, for what the pages show, and
/// changes the settings through it. Every change goes through the service; the app writes no
/// settings file of its own.</summary>
public sealed partial class DsaService
{
    private readonly IDsaCli _cli;

    public DsaService(IDsaCli cli)
    {
        _cli = cli;
    }

    private async Task<JsonElement> RunJsonAsync(params string[] args)
    {
        var all = new List<string>(args) { "--json" };
        var result = await _cli.RunAsync(all).ConfigureAwait(true);
        if (result.ExitCode != 0)
        {
            var detail = result.Error.StartsWith("dsa: ", StringComparison.Ordinal)
                ? result.Error["dsa: ".Length..]
                : result.Error;
            var problem = detail.Contains("cannot reach the DSA service", StringComparison.Ordinal)
                ? Problem.ServiceUnreachable
                : Problem.Refused;
            throw new DsaException(problem, detail.Length > 0 ? detail : $"dsa ended with code {result.ExitCode}");
        }
        try
        {
            using var document = JsonDocument.Parse(result.Output);
            return document.RootElement.Clone();
        }
        catch (JsonException e)
        {
            throw new DsaException(Problem.Unreadable, $"dsa printed something other than JSON: {e.Message}");
        }
    }

    private static string? OptionalString(JsonElement element, string name) =>
        element.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String
            ? value.GetString()
            : null;

    private static string String(JsonElement element, string name) => OptionalString(element, name) ?? "";

    private static bool Bool(JsonElement element, string name) =>
        element.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.True;

    /// <summary>The switch <paramref name="name"/>, or <paramref name="missing"/> when a service
    /// from before it existed does not report it.</summary>
    private static bool Bool(JsonElement element, string name, bool missing) =>
        element.TryGetProperty(name, out var value)
            ? value.ValueKind == JsonValueKind.True
            : missing;

    private static ulong Number(JsonElement element, string name) =>
        element.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.Number
            ? value.GetUInt64()
            : 0;

    public static SessionState ParseState(string? state) => state switch
    {
        "Stopped" => SessionState.Stopped,
        "Starting" => SessionState.Starting,
        "Active" => SessionState.Active,
        "Dozing" => SessionState.Dozing,
        "Frozen" => SessionState.Frozen,
        "Stopping" => SessionState.Stopping,
        "Stalled" => SessionState.Stalled,
        _ => SessionState.Unknown,
    };

    private static StatusInfo ReadStatus(JsonElement json) => new(
        ParseState(OptionalString(json, "state")),
        OptionalString(json, "last_reason"),
        OptionalString(json, "update_notice"),
        OptionalString(json, "problem"));

    public async Task<StatusInfo> StatusAsync() => ReadStatus(await RunJsonAsync("status").ConfigureAwait(true));

    /// <summary>Starts Android and waits until it has started or failed.</summary>
    public async Task<StatusInfo> StartAsync() =>
        ReadStatus(await RunJsonAsync("start", "--wait", "--timeout", "360").ConfigureAwait(true));

    /// <summary>Stops Android and waits until it has stopped.</summary>
    public async Task<StatusInfo> StopAsync() =>
        ReadStatus(await RunJsonAsync("stop", "--wait", "--timeout", "120").ConfigureAwait(true));

    private static SettingsInfo ReadSettings(JsonElement json) => new(
        String(json, "render_gpu"),
        Bool(json, "freeze"),
        Bool(json, "program_updates"),
        Bool(json, "image_updates"),
        Bool(json, "program_updates_available"),
        Bool(json, "image_updates_available"),
        String(json, "version"),
        OptionalString(json, "log_dir"),
        Bool(json, "clipboard", missing: true),
        Bool(json, "links_in_android"),
        OptionalString(json, "android_version"),
        Bool(json, "developer_mode"),
        Int(json, "adb_port") is > 0 and var port ? port : SettingsInfo.DefaultAdbPort,
        Bool(json, "input_trace"),
        Bool(json, "resize_trace"),
        Int(json, "memory_limit_mb"),
        Bool(json, "arm_translation", missing: true),
        Bool(json, "stop_without_asking"),
        Bool(json, "advanced_networking"),
        json.TryGetProperty("network", out var network) && network.ValueKind == JsonValueKind.Object
            ? ReadNetwork(network)
            : null,
        Bool(json, "file_sharing", missing: true),
        json.TryGetProperty("shared_folders", out var shared) && shared.ValueKind == JsonValueKind.Array
            ? Strings(json, "shared_folders")
            : null,
        Bool(json, "adb_auto_connect", missing: true),
        ReadAdbAutoConnectState(OptionalString(json, "adb_auto_connect_state")),
        Bool(json, "prerelease_updates"),
        Bool(json, "update_channel_available"),
        Bool(json, "touch_gestures_off", missing: true),
        ReadTouchGesturesState(OptionalString(json, "touch_gestures_state")),
        OptionalString(json, "touch_gestures_problem") is { Length: > 0 } problem ? problem : null);

    /// <summary>The touch gestures state a name stands for. An empty or unknown name, as an
    /// older service gives, is unknown.</summary>
    public static TouchGesturesState ReadTouchGesturesState(string? name) => name switch
    {
        "unsupported" => TouchGesturesState.Unsupported,
        "available" => TouchGesturesState.Available,
        "failed" => TouchGesturesState.Failed,
        _ => TouchGesturesState.Unknown,
    };

    /// <summary>The auto-connect state a name stands for. An empty or unknown name, as an older
    /// service gives, is off.</summary>
    public static AdbAutoConnectState ReadAdbAutoConnectState(string? name) => name switch
    {
        "waiting-for-server" => AdbAutoConnectState.WaitingForServer,
        "waiting-for-android" => AdbAutoConnectState.WaitingForAndroid,
        "attached" => AdbAutoConnectState.Attached,
        "detached" => AdbAutoConnectState.Detached,
        _ => AdbAutoConnectState.Off,
    };

    private static IReadOnlyList<string> Strings(JsonElement element, string name) =>
        element.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.Array
            ? value.EnumerateArray()
                .Where(item => item.ValueKind == JsonValueKind.String)
                .Select(item => item.GetString() ?? "")
                .ToList()
            : Array.Empty<string>();

    /// <summary>The network as <c>dsa network --json</c> and the settings print it.</summary>
    public static NetworkInfo ReadNetwork(JsonElement json) => new(
        OptionalString(json, "mode") switch
        {
            "nat" => NetworkMode.Nat,
            "advanced" => NetworkMode.Advanced,
            "off" => NetworkMode.Off,
            _ => NetworkMode.Unknown,
        },
        Strings(json, "addresses"),
        Strings(json, "dns_servers"),
        String(json, "gateway"),
        OptionalString(json, "problem") is { Length: > 0 } problem ? problem : null);

    /// <summary>Android's network as it stands now.</summary>
    public async Task<NetworkInfo> NetworkAsync() =>
        ReadNetwork(await RunJsonAsync("network").ConfigureAwait(true));

    /// <summary>Puts Android on this computer's network, or back behind NAT, from its next
    /// start.</summary>
    public Task<SettingsInfo> SetAdvancedNetworkingAsync(bool on) =>
        SetAsync("advanced-networking", on ? "on" : "off");

    public async Task<SettingsInfo> SettingsAsync() =>
        ReadSettings(await RunJsonAsync("settings").ConfigureAwait(true));

    private async Task<SettingsInfo> SetAsync(string name, string value) =>
        ReadSettings(await RunJsonAsync("settings", "set", name, value).ConfigureAwait(true));

    public Task<SettingsInfo> SetRenderGpuAsync(string value) => SetAsync("render-gpu", value);

    public Task<SettingsInfo> SetFreezeAsync(bool on) => SetAsync("freeze", on ? "on" : "off");

    public Task<SettingsInfo> SetLinksInAndroidAsync(bool on) => SetAsync("links-in-android", on ? "on" : "off");

    public Task<SettingsInfo> SetClipboardAsync(bool on) => SetAsync("clipboard", on ? "on" : "off");

    public Task<SettingsInfo> SetDeveloperModeAsync(bool on) => SetAsync("developer-mode", on ? "on" : "off");

    /// <summary>Whether developer mode's port is attached to this computer's adb server by
    /// itself.</summary>
    public Task<SettingsInfo> SetAdbAutoConnectAsync(bool on) => SetAsync("adb-auto-connect", on ? "on" : "off");

    /// <summary>Whether Windows' three- and four-finger touch gestures are off while a DSA app
    /// window is in front.</summary>
    public Task<SettingsInfo> SetTouchGesturesOffAsync(bool on) => SetAsync("touch-gestures-off", on ? "on" : "off");

    /// <summary>Whether stopping an app from the notification area skips the question that
    /// confirms it.</summary>
    public Task<SettingsInfo> SetStopWithoutAskingAsync(bool on) =>
        SetAsync("stop-without-asking", on ? "on" : "off");

    /// <summary>Erases the user's Android data: the service stops Android and puts a fresh data
    /// disk in place of theirs. The Android image and the settings stay.</summary>
    public async Task ResetDataAsync() =>
        await RunJsonAsync("reset-data", "--yes").ConfigureAwait(true);

    public Task<SettingsInfo> SetAdbPortAsync(int port) =>
        SetAsync("adb-port", port.ToString(System.Globalization.CultureInfo.InvariantCulture));

    public Task<SettingsInfo> SetArmTranslationAsync(bool on) => SetAsync("arm-translation", on ? "on" : "off");

    /// <summary>Whether the chosen Windows folders show in Android's shared storage.</summary>
    public Task<SettingsInfo> SetFileSharingAsync(bool on) => SetAsync("file-sharing", on ? "on" : "off");

    /// <summary>Which Windows folders are shared, in the order the service keeps them.</summary>
    public Task<SettingsInfo> SetSharedFoldersAsync(IEnumerable<string> folders)
    {
        var list = string.Join(",", folders);
        return SetAsync("shared-folders", list.Length == 0 ? "none" : list);
    }

    /// <summary>Opens Android's shared storage in Explorer, starting Android when it is not
    /// running, and returns the path Explorer opened.</summary>
    public async Task<string> OpenAndroidFilesAsync()
    {
        var json = await RunJsonAsync("files", "open").ConfigureAwait(true);
        return String(json, "path");
    }

    private static ArmPackageInfo? ReadArmPackage(JsonElement json, string name) =>
        json.TryGetProperty(name, out var p) && p.ValueKind == JsonValueKind.Object
            ? new ArmPackageInfo(String(p, "name"), String(p, "version"), String(p, "library"), String(p, "license"))
            : null;

    private static ArmTranslationInfo ReadArmTranslation(JsonElement json)
    {
        var package = ReadArmPackage(json, "package");
        var enabled = Bool(json, "enabled", missing: true);
        ArmGuestInfo? android = null;
        if (json.TryGetProperty("android", out var a) && a.ValueKind == JsonValueKind.Object)
        {
            android = new ArmGuestInfo(Bool(a, "installed"), Bool(a, "active"), String(a, "abilist"));
        }
        ArmTranslatorInfo? translator = null;
        if (json.TryGetProperty("translator", out var t))
        {
            if (t.ValueKind == JsonValueKind.Object)
            {
                ArmSource? source = String(t, "source") switch
                {
                    "default" => ArmSource.Default,
                    "user" => ArmSource.User,
                    "google" => ArmSource.Google,
                    _ => (ArmSource?)null,
                };
                if (source is { } known)
                {
                    translator = new ArmTranslatorInfo(known, String(t, "name"), String(t, "version"),
                        String(t, "license"));
                }
            }
        }
        else if (package is not null && enabled)
        {
            // A dsa from before the default translator: the installed package, while on.
            translator = new ArmTranslatorInfo(ArmSource.User, package.Name, package.Version, package.License);
        }
        return new ArmTranslationInfo(package, enabled, Bool(json, "supported"),
            android, Bool(json, "restart_needed"), Bool(json, "google_fetch_available"),
            ReadArmPackage(json, "default"), translator);
    }

    /// <summary>Whether ARM apps can run, and with which translation package.</summary>
    public async Task<ArmTranslationInfo> ArmTranslationAsync() =>
        ReadArmTranslation(await RunJsonAsync("arm-translation", "status").ConfigureAwait(true));

    /// <summary>Removes the ARM translation package; Android goes without it from its next
    /// start, back to the default translator where the image has one.</summary>
    public async Task<ArmTranslationInfo> RemoveArmTranslationAsync() =>
        ReadArmTranslation(await RunJsonAsync("arm-translation", "remove").ConfigureAwait(true));

    /// <summary>Goes back to the default translator the image carries, removing the package
    /// installed in its place. It applies from the next start of Android.</summary>
    public async Task<ArmTranslationInfo> UseDefaultArmTranslatorAsync() =>
        ReadArmTranslation(await RunJsonAsync("arm-translation", "use-default").ConfigureAwait(true));

    public Task<SettingsInfo> SetProgramUpdatesAsync(bool on) => SetAsync("program-updates", on ? "on" : "off");

    /// <summary>Chooses the release channel: on takes pre-releases as well as stable
    /// releases. Moving to the stable channel never installs a lower version.</summary>
    public Task<SettingsInfo> SetPrereleaseUpdatesAsync(bool on) => SetAsync("prerelease-updates", on ? "on" : "off");

    public Task<SettingsInfo> SetImageUpdatesAsync(bool on) => SetAsync("image-updates", on ? "on" : "off");

    public Task<SettingsInfo> SetInputTraceAsync(bool on) => SetAsync("input-trace", on ? "on" : "off");

    public Task<SettingsInfo> SetResizeTraceAsync(bool on) => SetAsync("resize-trace", on ? "on" : "off");

    /// <summary>Sets the most memory the user's Android may grow to, in MiB; 0 is automatic.</summary>
    public Task<SettingsInfo> SetMemoryLimitAsync(int mb) =>
        SetAsync("memory-limit", mb == 0 ? "auto" : mb.ToString(System.Globalization.CultureInfo.InvariantCulture));

    /// <summary>Freezes Android now: every process stops until one of its windows is shown
    /// again.</summary>
    public async Task<StatusInfo> FreezeAsync() => ReadStatus(await RunJsonAsync("freeze").ConfigureAwait(true));

    private static HostRole? ParseRole(string? role) => role switch
    {
        "vm" => HostRole.Vm,
        "window manager" => HostRole.WindowManager,
        "graphics renderer" => HostRole.Renderer,
        "app window" => HostRole.AppWindow,
        _ => null,
    };

    public async Task<MemoryReport> MemoryAsync()
    {
        var json = await RunJsonAsync("memory").ConfigureAwait(true);
        var host = new List<HostProcess>();
        if (json.TryGetProperty("host", out var list) && list.ValueKind == JsonValueKind.Array)
        {
            foreach (var p in list.EnumerateArray())
            {
                if (ParseRole(OptionalString(p, "role")) is { } role)
                {
                    host.Add(new HostProcess(role, (uint)Number(p, "pid"), Number(p, "working_set_kb"), Number(p, "private_kb")));
                }
            }
        }
        GuestMemoryInfo? guest = null;
        if (json.TryGetProperty("guest", out var g) && g.ValueKind == JsonValueKind.Object)
        {
            var top = new List<GuestProcess>();
            if (g.TryGetProperty("top_processes", out var processes) && processes.ValueKind == JsonValueKind.Array)
            {
                foreach (var p in processes.EnumerateArray())
                {
                    var pid = p.TryGetProperty("pid", out var id) && id.ValueKind == JsonValueKind.Number ? id.GetInt32() : 0;
                    top.Add(new GuestProcess(pid, String(p, "name"), Number(p, "pss_kb"), Number(p, "swap_pss_kb")));
                }
            }
            guest = new GuestMemoryInfo(
                Number(g, "mem_total_kb"), Number(g, "mem_free_kb"), Number(g, "mem_available_kb"),
                Number(g, "buffers_kb"), Number(g, "cached_kb"), Number(g, "anon_pages_kb"),
                Number(g, "shmem_kb"), Number(g, "slab_reclaimable_kb"), Number(g, "swap_total_kb"),
                Number(g, "swap_free_kb"), Number(g, "zram_original_kb"), Number(g, "zram_used_kb"),
                Number(g, "total_pss_kb"), top, Number(g, "age_seconds"), ReadKernel(g));
        }
        return new MemoryReport(guest, host, Number(json, "host_total_kb"), OptionalNumber(json, "vm_beyond_guest_kb"));
    }

    private static ulong? OptionalNumber(JsonElement element, string name) =>
        element.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.Number
            ? value.GetUInt64()
            : null;

    /// <summary>The kernel's own memory, or null from a service that does not report it.</summary>
    private static GuestKernelInfo? ReadKernel(JsonElement guest)
    {
        if (!guest.TryGetProperty("kernel", out var k) || k.ValueKind != JsonValueKind.Object)
        {
            return null;
        }
        return new GuestKernelInfo(
            Number(k, "slab_unreclaimable_kb"), Number(k, "kernel_stack_kb"), Number(k, "page_tables_kb"),
            Number(k, "vmalloc_used_kb"), Number(k, "percpu_kb"), Number(k, "cma_total_kb"),
            Number(k, "mem_online_kb"), Number(k, "unaccounted_kb"), OptionalNumber(k, "reserved_at_boot_kb"));
    }

    public async Task<IReadOnlyList<GpuInfo>> GpusAsync()
    {
        var json = await RunJsonAsync("gpus").ConfigureAwait(true);
        var gpus = new List<GpuInfo>();
        if (json.TryGetProperty("gpus", out var list) && list.ValueKind == JsonValueKind.Array)
        {
            foreach (var gpu in list.EnumerateArray())
            {
                gpus.Add(new GpuInfo(String(gpu, "name"), String(gpu, "luid"), Bool(gpu, "primary_display")));
            }
        }
        return gpus;
    }

    /// <summary>The start of dsa's line for an APK Android refused: the reason's code, then the
    /// detail.</summary>
    private static readonly Regex Refusal = new(@"^not installed \(([a-z-]+)\): (.*)$", RegexOptions.Singleline);

    /// <summary>Installs the APK at <paramref name="path"/> in Android, starting Android first
    /// when it is not running. A refusal by Android is a report with its reason; anything else
    /// that keeps the APK from Android is a <see cref="DsaException"/>.</summary>
    public async Task<InstallReport> InstallAsync(string path)
    {
        var result = await _cli.RunAsync(new[] { "install", path, "--json" }).ConfigureAwait(true);
        if (result.ExitCode != 0)
        {
            var detail = result.Error.StartsWith("dsa: ", StringComparison.Ordinal)
                ? result.Error["dsa: ".Length..]
                : result.Error;
            var refused = Refusal.Match(detail);
            if (refused.Success)
            {
                return new InstallReport(false, refused.Groups[1].Value, null, refused.Groups[2].Value);
            }
            var problem = detail.Contains("cannot reach the DSA service", StringComparison.Ordinal)
                ? Problem.ServiceUnreachable
                : Problem.Refused;
            throw new DsaException(problem, detail.Length > 0 ? detail : $"dsa ended with code {result.ExitCode}");
        }
        try
        {
            using var document = JsonDocument.Parse(result.Output);
            var json = document.RootElement;
            return new InstallReport(Bool(json, "installed"), String(json, "status"),
                OptionalString(json, "package"), String(json, "message"));
        }
        catch (JsonException e)
        {
            throw new DsaException(Problem.Unreadable, $"dsa printed something other than JSON: {e.Message}");
        }
    }

    private static int Int(JsonElement element, string name) =>
        element.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.Number &&
        value.TryGetInt32(out var number)
            ? number
            : 0;

    private static byte[] Base64(JsonElement element, string name)
    {
        try
        {
            return Convert.FromBase64String(String(element, name));
        }
        catch (FormatException)
        {
            return Array.Empty<byte>();
        }
    }

    internal static PlacedWidget ReadPlaced(JsonElement json)
    {
        var placement = json.TryGetProperty("placement", out var p) && p.ValueKind == JsonValueKind.Object
            ? p
            : default;
        return new PlacedWidget(Int(json, "id"), String(json, "provider"), String(json, "label"), String(json, "app"),
            placement.ValueKind == JsonValueKind.Object ? Int(placement, "x") : 0,
            placement.ValueKind == JsonValueKind.Object ? Int(placement, "y") : 0,
            placement.ValueKind == JsonValueKind.Object ? Int(placement, "width_dp") : 0,
            placement.ValueKind == JsonValueKind.Object ? Int(placement, "height_dp") : 0);
    }

    /// <summary>Reads one line of <c>dsa widgets watch --json</c>; null for a line that is no
    /// change it knows.</summary>
    public static WidgetChange? ParseWidgetChange(string line)
    {
        try
        {
            using var document = JsonDocument.Parse(line);
            var json = document.RootElement;
            if (json.ValueKind != JsonValueKind.Object)
            {
                return null;
            }
            return String(json, "event") switch
            {
                "widgets" => new WidgetChange.All(Items(json, "widgets").Select(ReadPlaced).ToList()),
                "placed" when json.TryGetProperty("widget", out var widget) && widget.ValueKind == JsonValueKind.Object =>
                    new WidgetChange.Placed(ReadPlaced(widget)),
                "removed" => new WidgetChange.Removed(Int(json, "id")),
                _ => null,
            };
        }
        catch (JsonException)
        {
            return null;
        }
    }

    private static IEnumerable<JsonElement> Items(JsonElement element, string name) =>
        element.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.Array
            ? value.EnumerateArray()
            : Enumerable.Empty<JsonElement>();

    /// <summary>The widgets the apps in Android offer and the ones placed on the desktop,
    /// starting Android first when it is not running.</summary>
    public async Task<WidgetList> WidgetsAsync()
    {
        var json = await RunJsonAsync("widgets", "list").ConfigureAwait(true);
        var offers = Items(json, "providers")
            .Select(p => new WidgetOffer(String(p, "provider"), String(p, "package"), String(p, "label"),
                Base64(p, "preview_png"), Int(p, "default_width_dp"), Int(p, "default_height_dp"),
                Bool(p, "needs_configuration"), String(p, "app")))
            .ToList();
        var placed = Items(json, "widgets").Select(ReadPlaced).ToList();
        return new WidgetList(offers, placed);
    }

    /// <summary>Places a widget of <paramref name="provider"/> on the desktop. When its app asks
    /// for a setup, this waits for the user to finish it.</summary>
    public async Task<PlacedWidget> AddWidgetAsync(string provider) =>
        ReadPlaced(await RunJsonAsync("widgets", "add", provider).ConfigureAwait(true));

    /// <summary>Removes the widget <paramref name="id"/> from the desktop.</summary>
    public async Task RemoveWidgetAsync(int id) =>
        await RunJsonAsync("widgets", "remove", id.ToString(System.Globalization.CultureInfo.InvariantCulture))
            .ConfigureAwait(true);

    /// <summary>The apps that run in the background in Android, and its media sessions. It
    /// never starts Android; one that is not running has none.</summary>
    public async Task<BackgroundList> BackgroundAsync()
    {
        var json = await RunJsonAsync("background", "list").ConfigureAwait(true);
        var apps = Items(json, "apps")
            .Select(a => new BackgroundApp(String(a, "package"), String(a, "label"), String(a, "title"),
                String(a, "text"),
                Items(a, "actions").Where(t => t.ValueKind == JsonValueKind.String)
                    .Select(t => t.GetString() ?? "").ToList(),
                Base64(a, "icon_png")))
            .ToList();
        var media = Items(json, "media")
            .Select(m => new MediaInfo(String(m, "package"), String(m, "label"), String(m, "title"),
                String(m, "artist"), String(m, "album"), String(m, "playback")))
            .ToList();
        return new BackgroundList(apps, media);
    }

    /// <summary>Stops <paramref name="package"/>: Android force-stops it, which ends what it
    /// does in the background and frees its memory.</summary>
    public async Task StopBackgroundAsync(string package) =>
        await RunJsonAsync("background", "stop", package).ConfigureAwait(true);

    /// <summary>Opens the window of <paramref name="package"/>, which runs in the
    /// background.</summary>
    public async Task OpenBackgroundAsync(string package) =>
        await RunJsonAsync("background", "open", package).ConfigureAwait(true);

    /// <summary>Saves the diagnostics of Android and of DSA on this computer to
    /// <paramref name="path"/>, as one zip archive. Returns what dsa said.</summary>
    public async Task<string> CollectDiagnosticsAsync(string path)
    {
        var result = await _cli.RunAsync(new[] { "logs", "collect", "-o", path }).ConfigureAwait(true);
        if (result.ExitCode != 0)
        {
            var detail = result.Error.StartsWith("dsa: ", StringComparison.Ordinal) ? result.Error[5..] : result.Error;
            var problem = detail.Contains("cannot reach the DSA service", StringComparison.Ordinal)
                ? Problem.ServiceUnreachable
                : Problem.Refused;
            throw new DsaException(problem, detail.Length > 0 ? detail : $"dsa ended with code {result.ExitCode}");
        }
        return result.Output;
    }

    /// <summary>Installs the newest version of DSA now. Returns what the service said.</summary>
    public async Task<string> UpdateNowAsync()
    {
        var result = await _cli.RunAsync(new[] { "update" }).ConfigureAwait(true);
        if (result.ExitCode != 0)
        {
            var detail = result.Error.StartsWith("dsa: ", StringComparison.Ordinal) ? result.Error[5..] : result.Error;
            throw new DsaException(Problem.Refused, detail);
        }
        return result.Output;
    }
}
