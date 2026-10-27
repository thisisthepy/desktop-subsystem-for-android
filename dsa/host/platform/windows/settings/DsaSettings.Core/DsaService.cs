using System.Text.Json;

namespace Dsa.Settings.Core;

/// <summary>Asks the DSA service, through the dsa command, for what the pages show, and
/// changes the settings through it. Every change goes through the service; the app writes no
/// settings file of its own.</summary>
public sealed class DsaService
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
        _ => SessionState.Unknown,
    };

    private static StatusInfo ReadStatus(JsonElement json) => new(
        ParseState(OptionalString(json, "state")),
        OptionalString(json, "last_reason"),
        OptionalString(json, "update_notice"));

    public async Task<StatusInfo> StatusAsync() => ReadStatus(await RunJsonAsync("status").ConfigureAwait(true));

    /// <summary>Starts Android and waits until it has started or failed.</summary>
    public async Task<StatusInfo> StartAsync() =>
        ReadStatus(await RunJsonAsync("start", "--wait", "--timeout", "180").ConfigureAwait(true));

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
        OptionalString(json, "log_dir"));

    public async Task<SettingsInfo> SettingsAsync() =>
        ReadSettings(await RunJsonAsync("settings").ConfigureAwait(true));

    private async Task<SettingsInfo> SetAsync(string name, string value) =>
        ReadSettings(await RunJsonAsync("settings", "set", name, value).ConfigureAwait(true));

    public Task<SettingsInfo> SetRenderGpuAsync(string value) => SetAsync("render-gpu", value);

    public Task<SettingsInfo> SetFreezeAsync(bool on) => SetAsync("freeze", on ? "on" : "off");

    public Task<SettingsInfo> SetProgramUpdatesAsync(bool on) => SetAsync("program-updates", on ? "on" : "off");

    public Task<SettingsInfo> SetImageUpdatesAsync(bool on) => SetAsync("image-updates", on ? "on" : "off");

    private static HostRole? ParseRole(string? role) => role switch
    {
        "vm" => HostRole.Vm,
        "window manager" => HostRole.WindowManager,
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
                Number(g, "total_pss_kb"), top, Number(g, "age_seconds"));
        }
        return new MemoryReport(guest, host, Number(json, "host_total_kb"));
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
