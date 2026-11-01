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
        "Install_Installed" => "installed {0} as {1}",
        "Install_Refused" => "refused {0}: {1} [{2}]",
        "Install_NotSent" => "not sent {0}: {1}",
        "Install_NotApk" => "not an APK: {0}",
        "Install_Busy" => "installing {0}",
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
    public async Task Fr17_general_page_shows_what_keeps_a_stalled_android_from_being_ready()
    {
        var cli = new FakeCli()
            .Answer("status --json", "{\"state\":\"Stalled\",\"last_reason\":null,\"update_notice\":null,\"problem\":\"the agent cannot reach the channel\"}");
        var vm = new GeneralViewModel(new DsaService(cli), new KeyText());
        await vm.RefreshAsync();
        Assert.Equal(SessionState.Stalled, vm.State);
        Assert.Equal("State_Stalled", vm.StateText);
        Assert.True(vm.HasProblem);
        Assert.Equal("the agent cannot reach the channel", vm.Problem);
        Assert.False(vm.CanStart);
        Assert.True(vm.CanStop);
    }

    [Fact]
    public async Task Fr17_general_page_shows_the_state_and_the_last_error()
    {
        var cli = new FakeCli()
            .Answer("status --json", "{\"state\":\"Stopped\",\"last_reason\":\"the VM exited\",\"update_notice\":null}")
            .Answer("start --wait --timeout 360 --json", "{\"state\":\"Active\",\"last_reason\":null,\"update_notice\":null}");
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
        // The update switches apply to every user: only an administrator changes them here.
        Assert.False(vm.CanChangeProgramUpdates);
        Assert.True(vm.ProgramUpdatesNeedAdministrator);
        Assert.True(vm.NeedsAdministrator);
        Assert.True(vm.CanUpdateNow);
        vm.IsAdministrator = true;
        Assert.True(vm.CanChangeProgramUpdates);
        Assert.False(vm.ProgramUpdatesNeedAdministrator);
        Assert.False(vm.CanChangeImageUpdates, "this installation does not update images");
        Assert.False(vm.ImageUpdatesNeedAdministrator);
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
    public async Task Fr9_the_links_switch_is_off_by_default_and_changed_through_the_service()
    {
        var withLinks = Settings.Replace("\"freeze\":false", "\"freeze\":false,\"links_in_android\":false");
        var cli = new FakeCli()
            .Answer("settings --json", withLinks)
            .Fail("gpus --json", "dsa: no GPU list")
            .Answer("settings set links-in-android on --json",
                withLinks.Replace("\"links_in_android\":false", "\"links_in_android\":true"))
            .Answer("settings set links-in-android off --json", withLinks);
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.False(vm.LinksInAndroid);

        await vm.SetLinksInAndroidAsync(true);
        Assert.True(vm.LinksInAndroid);
        Assert.False(vm.Freeze, "one change keeps the others");
        await vm.SetLinksInAndroidAsync(false);
        Assert.False(vm.LinksInAndroid);
        Assert.Contains("settings set links-in-android on --json", cli.Calls);
        Assert.Contains("settings set links-in-android off --json", cli.Calls);
    }

    [Fact]
    public async Task Fr9_a_service_that_does_not_report_the_links_switch_opens_links_on_the_host()
    {
        var cli = new FakeCli()
            .Answer("settings --json", Settings)
            .Fail("gpus --json", "dsa: no GPU list");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.False(vm.LinksInAndroid);
    }

    [Fact]
    public async Task Fr13_the_clipboard_switch_is_on_by_default_and_changed_through_the_service()
    {
        var withClipboard = Settings.Replace("\"freeze\":false", "\"freeze\":false,\"clipboard\":true");
        var cli = new FakeCli()
            .Answer("settings --json", withClipboard)
            .Fail("gpus --json", "dsa: no GPU list")
            .Answer("settings set clipboard off --json", withClipboard.Replace("\"clipboard\":true", "\"clipboard\":false"))
            .Answer("settings set clipboard on --json", withClipboard);
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.True(vm.Clipboard);
        Assert.True(vm.CanChange);

        await vm.SetClipboardAsync(false);
        Assert.False(vm.Clipboard);
        Assert.False(vm.Freeze, "one change keeps the others");
        await vm.SetClipboardAsync(true);
        Assert.True(vm.Clipboard);
        Assert.Contains("settings set clipboard off --json", cli.Calls);
        Assert.Contains("settings set clipboard on --json", cli.Calls);
    }

    [Fact]
    public async Task Fr13_a_service_that_does_not_report_the_clipboard_switch_shares_it()
    {
        var cli = new FakeCli()
            .Answer("settings --json", Settings)
            .Fail("gpus --json", "dsa: no GPU list");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.True(vm.Clipboard);
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

    [Fact]
    public async Task Fr11_dropping_an_apk_installs_it_and_shows_the_package()
    {
        var cli = new FakeCli()
            .Answer(@"install C:\Downloads\notes.apk --json",
                "{\"installed\":true,\"status\":\"installed\",\"package\":\"com.example.notes\",\"message\":\"\"}");
        var vm = new InstallViewModel(new DsaService(cli), new KeyText());
        Assert.True(vm.Accepts(new[] { @"C:\Downloads\notes.apk" }));
        await vm.DropAsync(new[] { @"C:\Downloads\notes.apk" });
        Assert.Equal(new[] { @"install C:\Downloads\notes.apk --json" }, cli.Calls);
        Assert.True(vm.IsOpen);
        Assert.False(vm.IsBusy);
        Assert.Equal(InstallSeverity.Success, vm.Severity);
        Assert.Equal("Install_DoneTitle", vm.Title);
        Assert.Equal("installed notes.apk as com.example.notes", vm.Message);
    }

    [Fact]
    public async Task Fr11_a_refused_apk_shows_its_reason_in_the_users_language()
    {
        var cli = new FakeCli()
            .Fail(@"install C:\Downloads\notes.apk --json",
                "dsa: not installed (conflicting-signature): notes.apk: The app is installed already. Android said: INSTALL_FAILED_UPDATE_INCOMPATIBLE");
        var vm = new InstallViewModel(new DsaService(cli), new KeyText());
        await vm.DropAsync(new[] { @"C:\Downloads\notes.apk" });
        Assert.Equal(InstallSeverity.Error, vm.Severity);
        Assert.Equal("Install_FailedTitle", vm.Title);
        Assert.StartsWith("refused notes.apk: Install_Reason_conflicting-signature [", vm.Message);
        Assert.Contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE", vm.Message);
    }

    [Fact]
    public async Task Fr11_files_that_are_not_apks_are_not_sent()
    {
        var cli = new FakeCli();
        var vm = new InstallViewModel(new DsaService(cli), new KeyText());
        Assert.False(vm.Accepts(new[] { @"C:\notes.txt" }));
        Assert.False(vm.Accepts(null));
        Assert.True(vm.Accepts(new[] { @"C:\notes.txt", @"C:\app.APK" }));
        await vm.DropAsync(new[] { @"C:\notes.txt" });
        Assert.Empty(cli.Calls);
        Assert.Equal(InstallSeverity.Error, vm.Severity);
        Assert.Equal("not an APK: notes.txt", vm.Message);
    }

    [Fact]
    public async Task Fr11_several_apks_are_installed_one_after_the_other()
    {
        const string Installed = "{\"installed\":true,\"status\":\"installed\",\"package\":\"a.b\",\"message\":\"\"}";
        var cli = new FakeCli()
            .Answer(@"install C:\one.apk --json", Installed)
            .Fail(@"install C:\two.apk --json", "dsa: not installed (invalid-apk): two.apk: The file is not a signed APK")
            .Answer(@"install C:\three.apk --json", Installed);
        var vm = new InstallViewModel(new DsaService(cli), new KeyText());
        await vm.DropAsync(new[] { @"C:\one.apk", @"C:\two.apk", @"C:\notes.txt", @"C:\three.apk" });
        Assert.Equal(new[]
        {
            @"install C:\one.apk --json",
            @"install C:\two.apk --json",
            @"install C:\three.apk --json",
        }, cli.Calls);
        Assert.Equal(InstallSeverity.Error, vm.Severity);
        var lines = vm.Message.Split('\n');
        Assert.Equal("installed one.apk as a.b", lines[0]);
        Assert.StartsWith("refused two.apk: Install_Reason_invalid-apk", lines[1]);
        Assert.Equal("installed three.apk as a.b", lines[2]);
        Assert.Equal("not an APK: notes.txt", lines[3]);
    }

    [Fact]
    public async Task Fr11_an_apk_that_does_not_reach_android_says_why()
    {
        var cli = new FakeCli()
            .Fail(@"install C:\notes.apk --json", "dsa: cannot reach the DSA service (no pipe)");
        var vm = new InstallViewModel(new DsaService(cli), new KeyText());
        await vm.DropAsync(new[] { @"C:\notes.apk" });
        Assert.Equal(InstallSeverity.Error, vm.Severity);
        Assert.Equal("not sent notes.apk: Problem_ServiceUnreachable (cannot reach the DSA service (no pipe))", vm.Message);
    }
}
