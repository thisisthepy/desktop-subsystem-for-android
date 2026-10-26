using System.Globalization;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>Answers each dsa command line with what the test set for it, and records the
/// command lines it was given.</summary>
internal sealed class FakeCli : IDsaCli
{
    private readonly Dictionary<string, CliResult> _answers = new();
    public List<string> Calls { get; } = new();

    public FakeCli Answer(string line, string json) => Set(line, new CliResult(0, json, ""));

    public FakeCli Fail(string line, string error) => Set(line, new CliResult(1, "", error));

    private FakeCli Set(string line, CliResult result)
    {
        _answers[line] = result;
        return this;
    }

    public Task<CliResult> RunAsync(IReadOnlyList<string> args, CancellationToken cancel = default)
    {
        var line = string.Join(" ", args);
        Calls.Add(line);
        return Task.FromResult(_answers.TryGetValue(line, out var result)
            ? result
            : new CliResult(2, "", $"dsa: the test has no answer for {line}"));
    }
}

internal sealed class KeyText : ITextSource
{
    public string Get(string key) => key switch
    {
        "Memory_ReportAge" => "{0} s",
        "Memory_ProcessDetail" => "pid {0}, {1} private",
        "Memory_ZramDetail" => "{0} stored",
        "Memory_GuestProcessDetail" => "pid {0}",
        _ => key,
    };
}

public class ViewModelTests
{
    private const string Settings =
        "{\"render_gpu\":\"display\",\"freeze\":false,\"program_updates\":true,\"image_updates\":true," +
        "\"program_updates_available\":true,\"image_updates_available\":false,\"version\":\"0.0.9\"," +
        "\"log_dir\":\"C:\\\\ProgramData\\\\DSA\\\\logs\"}";

    public ViewModelTests()
    {
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
    }

    [Fact]
    public async Task Fr17_general_page_shows_the_state_and_the_last_error()
    {
        var cli = new FakeCli()
            .Answer("status --json", "{\"state\":\"Stopped\",\"last_reason\":\"the VM exited\",\"update_notice\":null}")
            .Answer("start --wait --timeout 180 --json", "{\"state\":\"Active\",\"last_reason\":null,\"update_notice\":null}");
        var vm = new GeneralViewModel(new DsaService(cli), new KeyText());
        await vm.RefreshAsync();
        Assert.Equal(SessionState.Stopped, vm.State);
        Assert.Equal("State_Stopped", vm.StateText);
        Assert.True(vm.HasLastReason);
        Assert.Equal("the VM exited", vm.LastReason);
        Assert.True(vm.CanStart);
        Assert.False(vm.CanStop);

        await vm.StartAsync();
        Assert.Equal(SessionState.Active, vm.State);
        Assert.False(vm.CanStart);
        Assert.True(vm.CanStop);
        Assert.False(vm.HasLastReason);
    }

    [Fact]
    public async Task Fr17_general_page_says_when_the_service_cannot_be_reached()
    {
        var cli = new FakeCli().Fail("status --json",
            "dsa: cannot reach the DSA service (the pipe is not there); check that it is installed and running");
        var vm = new GeneralViewModel(new DsaService(cli), new KeyText());
        await vm.RefreshAsync();
        Assert.True(vm.HasError);
        Assert.StartsWith("Problem_ServiceUnreachable (cannot reach the DSA service", vm.Error);
        Assert.Equal(SessionState.Unknown, vm.State);
        Assert.False(vm.CanStart);
    }

    [Fact]
    public async Task Fr17_settings_are_changed_through_the_service_only()
    {
        var cli = new FakeCli()
            .Answer("settings --json", Settings)
            .Answer("gpus --json", "{\"gpus\":[{\"name\":\"Intel(R) Arc(TM) Graphics\",\"luid\":\"0:1\",\"primary_display\":true}," +
                                   "{\"name\":\"NVIDIA GeForce RTX 4050 Laptop GPU\",\"luid\":\"0:2\",\"primary_display\":false}]}")
            .Answer("settings set render-gpu NVIDIA GeForce RTX 4050 Laptop GPU --json",
                Settings.Replace("\"render_gpu\":\"display\"", "\"render_gpu\":\"NVIDIA GeForce RTX 4050 Laptop GPU\""))
            .Answer("settings set freeze on --json", Settings.Replace("\"freeze\":false", "\"freeze\":true"));
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.Equal("display", vm.RenderGpu);
        Assert.False(vm.Freeze);
        Assert.Equal("0.0.9", vm.ServiceVersion);
        Assert.Equal("C:\\ProgramData\\DSA\\logs", vm.LogDir);
        Assert.True(vm.CanChangeProgramUpdates);
        Assert.False(vm.CanChangeImageUpdates);
        Assert.Equal("Intel(R) Arc(TM) Graphics", vm.DisplayGpuName);
        Assert.Equal(
            new[] { "display", "integrated", "discrete", "Intel(R) Arc(TM) Graphics", "NVIDIA GeForce RTX 4050 Laptop GPU" },
            vm.GpuChoices.Select(c => c.Value));
        Assert.Equal(0, vm.SelectedGpuIndex);

        await vm.SetRenderGpuAsync("NVIDIA GeForce RTX 4050 Laptop GPU");
        Assert.Equal(4, vm.SelectedGpuIndex);
        await vm.SetFreezeAsync(true);
        Assert.True(vm.Freeze);
        Assert.All(cli.Calls, call => Assert.EndsWith("--json", call));
    }

    [Fact]
    public async Task Fr17_a_refused_setting_keeps_the_last_reported_value()
    {
        var cli = new FakeCli()
            .Answer("settings --json", Settings)
            .Fail("gpus --json", "dsa: no GPU list")
            .Fail("settings set image-updates off --json", "dsa: the settings could not be written");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.Equal(3, vm.GpuChoices.Count);
        await vm.SetImageUpdatesAsync(false);
        Assert.True(vm.ImageUpdates, "the switch moves back to what the service keeps");
        Assert.True(vm.HasError);
        Assert.Contains("could not be written", vm.Error);
    }

    [Fact]
    public async Task Fr17_a_gpu_name_chosen_before_still_shows_as_chosen()
    {
        var cli = new FakeCli()
            .Answer("settings --json", Settings.Replace("\"render_gpu\":\"display\"", "\"render_gpu\":\"RTX\""))
            .Answer("gpus --json", "{\"gpus\":[]}");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.Equal(RenderGpuKind.Named, vm.Settings!.RenderGpuKind);
        Assert.Equal("RTX", vm.GpuChoices[vm.SelectedGpuIndex].Value);
    }

    [Fact]
    public async Task Fr17_memory_page_shows_the_host_processes_and_the_guest_breakdown()
    {
        const string report =
            "{\"guest\":{\"mem_total_kb\":4194304,\"mem_free_kb\":1048576,\"mem_available_kb\":3145728," +
            "\"buffers_kb\":1024,\"cached_kb\":203776,\"anon_pages_kb\":0,\"shmem_kb\":0,\"slab_reclaimable_kb\":51200," +
            "\"swap_total_kb\":0,\"swap_free_kb\":0,\"zram_original_kb\":0,\"zram_used_kb\":0,\"total_pss_kb\":512000," +
            "\"age_seconds\":12,\"top_processes\":[{\"pid\":42,\"name\":\"system_server\",\"pss_kb\":153600,\"swap_pss_kb\":0}]}," +
            "\"host\":[{\"role\":\"vm\",\"pid\":10,\"working_set_kb\":921600,\"private_kb\":900000}," +
            "{\"role\":\"window manager\",\"pid\":20,\"working_set_kb\":40960,\"private_kb\":30000}]," +
            "\"host_total_kb\":962560}";
        var cli = new FakeCli().Answer("memory --json", report);
        var vm = new MemoryViewModel(new DsaService(cli), new KeyText());
        await vm.RefreshAsync();
        Assert.True(vm.HostRunning);
        Assert.Equal("940.0 MB", vm.HostTotal);
        Assert.Equal(new[] { "Role_Vm", "Role_WindowManager" }, vm.HostRows.Select(r => r.Label));
        Assert.Equal("900.0 MB", vm.HostRows[0].Size);
        Assert.True(vm.GuestReported);
        Assert.Equal("4.00 GB", vm.GuestTotal);
        Assert.Equal(25.0, vm.GuestUsedPercent, 3);
        Assert.Equal("12 s", vm.GuestAge);
        Assert.Equal("500.0 MB", vm.GuestRows.Single(r => r.Label == "Memory_Apps").Size);
        Assert.Equal("200.0 MB", vm.GuestRows.Single(r => r.Label == "Memory_Cache").Size);
        Assert.Equal("system_server", vm.GuestProcessRows.Single().Label);
    }

    [Fact]
    public async Task Fr17_memory_page_says_when_android_has_not_reported()
    {
        var cli = new FakeCli().Answer("memory --json", "{\"guest\":null,\"host\":[],\"host_total_kb\":0}");
        var vm = new MemoryViewModel(new DsaService(cli), new KeyText());
        await vm.RefreshAsync();
        Assert.False(vm.HostRunning);
        Assert.True(vm.GuestNotReported);
        Assert.Empty(vm.GuestRows);
    }

    [Fact]
    public async Task Fr17_text_that_is_not_json_is_reported()
    {
        var cli = new FakeCli().Answer("memory --json", "Host: 1 MB");
        var vm = new MemoryViewModel(new DsaService(cli), new KeyText());
        await vm.RefreshAsync();
        Assert.StartsWith("Problem_Unreadable", vm.Error);
    }

    [Fact]
    public void Fr17_sizes_read_as_megabytes_and_gigabytes()
    {
        Assert.Equal("0.5 MB", Format.Size(512, CultureInfo.InvariantCulture));
        Assert.Equal("1.50 GB", Format.Size(1572864, CultureInfo.InvariantCulture));
        Assert.Equal("1,50 GB", Format.Size(1572864, new CultureInfo("de-DE")));
    }
}
