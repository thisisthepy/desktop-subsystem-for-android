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
        "Widgets_Size" => "{0}x{1}",
        "Widgets_At" => "{0},{1} {2}x{3}",
        "Widgets_Placed" => "placed {0}",
        "Widgets_Removed" => "removed {0}",
        "Background_Playing" => "playing {0}",
        "Background_Paused" => "paused {0}",
        "Background_Stopped" => "stopped {0}",
        "TouchGestures_Failed" => "cannot be used: {0}",
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
        Assert.Equal("LastReason_General", vm.LastReason);
        Assert.Equal("the VM exited", vm.LastReasonDetail);
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
    public async Task Fr17_about_page_names_the_android_image_the_next_start_boots()
    {
        var cli = new FakeCli()
            .Answer("settings --json", Settings.Replace("}", ",\"android_version\":\"17.0.0-16373615-7\"}"))
            .Answer("gpus --json", "{\"gpus\":[]}");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.True(vm.HasAndroidVersion);
        Assert.Equal("17.0.0-16373615-7", vm.AndroidVersion);

        // A service that does not know the image (or an older one) leaves the line empty.
        var older = new SettingsViewModel(new DsaService(new FakeCli()
            .Answer("settings --json", Settings)
            .Answer("gpus --json", "{\"gpus\":[]}")), new KeyText());
        await older.LoadAsync();
        Assert.False(older.HasAndroidVersion);
        Assert.Equal("", older.AndroidVersion);
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
    public async Task Fr29_file_sharing_is_on_with_every_folder_by_default_and_changed_through_the_service()
    {
        var withSharing = Settings.Replace("\"freeze\":false",
            "\"freeze\":false,\"file_sharing\":true," +
            "\"shared_folders\":[\"Documents\",\"Downloads\",\"Pictures\",\"Music\",\"Videos\",\"Desktop\"]");
        var withoutMusic = withSharing.Replace("\"Music\",", "");
        var cli = new FakeCli()
            .Answer("settings --json", withSharing)
            .Fail("gpus --json", "dsa: no GPU list")
            .Answer("settings set shared-folders Documents,Downloads,Pictures,Videos,Desktop --json", withoutMusic)
            .Answer("settings set file-sharing off --json",
                withoutMusic.Replace("\"file_sharing\":true", "\"file_sharing\":false"));
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.True(vm.FileSharing);
        Assert.True(vm.ShareMusic);
        Assert.True(vm.CanChooseFolders);

        await vm.SetFolderSharedAsync("Music", false);
        Assert.False(vm.ShareMusic);
        Assert.True(vm.SharePictures, "one folder changes alone");

        await vm.SetFileSharingAsync(false);
        Assert.False(vm.FileSharing);
        Assert.False(vm.CanChooseFolders, "the folders wait while sharing is off");
        Assert.Contains("settings set file-sharing off --json", cli.Calls);
    }

    [Fact]
    public async Task Fr29_clearing_every_folder_shares_none_and_an_unknown_folder_is_not_sent()
    {
        var one = Settings.Replace("\"freeze\":false",
            "\"freeze\":false,\"file_sharing\":true,\"shared_folders\":[\"Pictures\"]");
        var cli = new FakeCli()
            .Answer("settings --json", one)
            .Fail("gpus --json", "dsa: no GPU list")
            .Answer("settings set shared-folders none --json", one.Replace("[\"Pictures\"]", "[]"));
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.False(vm.ShareDocuments);
        await vm.SetFolderSharedAsync("C:\\Windows", true);
        Assert.DoesNotContain(cli.Calls, call => call.Contains("C:"));
        await vm.SetFolderSharedAsync("Pictures", false);
        Assert.False(vm.SharePictures);
        Assert.Contains("settings set shared-folders none --json", cli.Calls);
    }

    [Fact]
    public async Task Fr29_a_service_that_does_not_report_file_sharing_shares_every_folder()
    {
        var cli = new FakeCli()
            .Answer("settings --json", Settings)
            .Fail("gpus --json", "dsa: no GPU list");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.True(vm.FileSharing);
        Assert.True(SettingsInfo.AllFolders.All(vm.IsFolderShared));
    }

    [Fact]
    public async Task Fr29_the_android_files_button_opens_them_through_the_service_and_shows_a_failure()
    {
        var cli = new FakeCli()
            .Answer("settings --json", Settings)
            .Fail("gpus --json", "dsa: no GPU list")
            .Answer("files open --json", "{\"url\":\"http://127.0.0.1:5/t/\",\"path\":\"\\\\\\\\127.0.0.1@5\\\\t\"}");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        await vm.OpenAndroidFilesAsync();
        Assert.Contains("files open --json", cli.Calls);
        Assert.False(vm.HasError);

        var failing = new FakeCli()
            .Answer("settings --json", Settings)
            .Fail("gpus --json", "dsa: no GPU list")
            .Fail("files open --json", "dsa: this host cannot reach Android's files");
        var broken = new SettingsViewModel(new DsaService(failing), new KeyText());
        await broken.LoadAsync();
        await broken.OpenAndroidFilesAsync();
        Assert.True(broken.HasError);
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
    public async Task Fr20_developer_mode_is_off_by_default_and_shows_the_adb_address_when_on()
    {
        var off = Settings.Replace("\"freeze\":false", "\"freeze\":false,\"developer_mode\":false,\"adb_port\":58526");
        var on = off.Replace("\"developer_mode\":false", "\"developer_mode\":true");
        var cli = new FakeCli()
            .Answer("settings --json", off)
            .Fail("gpus --json", "dsa: no GPU list")
            .Answer("settings set developer-mode on --json", on)
            .Answer("settings set adb-port 5037 --json", on.Replace("58526", "5037"));
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.False(vm.DeveloperMode);
        Assert.Equal(58526, vm.AdbPort);

        await vm.SetDeveloperModeAsync(true);
        Assert.True(vm.DeveloperMode);
        Assert.Equal("adb connect 127.0.0.1:58526", vm.AdbConnect);
        await vm.SetAdbPortAsync("70000");
        Assert.DoesNotContain(cli.Calls, call => call.Contains("adb-port 70000"));
        await vm.SetAdbPortAsync("5037");
        Assert.Equal("adb connect 127.0.0.1:5037", vm.AdbConnect);
        Assert.Contains("settings set developer-mode on --json", cli.Calls);
    }

    [Fact]
    public async Task Fr25_the_arm_section_says_whether_arm_apps_run_and_removes_the_package()
    {
        const string none =
            "{\"package\":null,\"enabled\":true,\"supported\":false," +
            "\"android\":{\"installed\":false,\"active\":false,\"abilist\":\"x86_64\"},\"restart_needed\":false}";
        const string installed =
            "{\"package\":{\"name\":\"Translator\",\"version\":\"1.0\",\"library\":\"libtranslator.so\"," +
            "\"abis\":[\"arm64-v8a\"],\"license\":\"Apache-2.0\"},\"enabled\":true,\"supported\":true," +
            "\"android\":{\"installed\":true,\"active\":false,\"abilist\":\"x86_64\"},\"restart_needed\":true}";
        var cli = new FakeCli()
            .Answer("settings --json", Settings)
            .Fail("gpus --json", "dsa: no GPU list")
            .Answer("arm-translation status --json", installed)
            .Answer("arm-translation remove --json", none)
            .Answer("settings set arm-translation off --json",
                Settings.Replace("\"freeze\":false", "\"freeze\":false,\"arm_translation\":false"));
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.True(vm.ArmTranslation, "on when the service does not say otherwise");
        Assert.True(vm.ArmSupported);
        Assert.Equal("Arm_Supported", vm.ArmStatus);
        Assert.Equal("Arm_Abis", vm.ArmAbis);
        Assert.True(vm.ArmRestartNeeded);
        Assert.True(vm.CanRemoveArmTranslation);

        await vm.SetArmTranslationAsync(false);
        Assert.False(vm.ArmTranslation);
        Assert.Contains("settings set arm-translation off --json", cli.Calls);

        await vm.RemoveArmTranslationAsync();
        Assert.False(vm.ArmSupported);
        Assert.False(vm.HasArmPackage);
        Assert.False(vm.CanRemoveArmTranslation);
        Assert.Equal("Arm_NotSupported", vm.ArmStatus);
        Assert.Contains("arm-translation remove --json", cli.Calls);
    }

    [Fact]
    public async Task Fr25_a_service_that_cannot_say_shows_arm_apps_as_not_supported()
    {
        var cli = new FakeCli()
            .Answer("settings --json", Settings)
            .Fail("gpus --json", "dsa: no GPU list")
            .Fail("arm-translation status --json", "dsa: unexpected \"arm-translation\"");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.False(vm.HasError);
        Assert.False(vm.ArmSupported);
        Assert.Equal("Arm_NotSupported", vm.ArmStatus);
        Assert.Equal("Arm_NotRunning", vm.ArmAbis);
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
    public async Task Nfr5_memory_page_shows_the_graphics_renderer_as_a_process_of_its_own()
    {
        const string report =
            "{\"host\":[{\"role\":\"vm\",\"pid\":10,\"working_set_kb\":921600,\"private_kb\":900000}," +
            "{\"role\":\"window manager\",\"pid\":20,\"working_set_kb\":40960,\"private_kb\":30000}," +
            "{\"role\":\"graphics renderer\",\"pid\":21,\"working_set_kb\":307200,\"private_kb\":290000}]," +
            "\"host_total_kb\":1269760}";
        var cli = new FakeCli().Answer("memory --json", report);
        var vm = new MemoryViewModel(new DsaService(cli), new KeyText());
        await vm.RefreshAsync();
        Assert.Equal(new[] { "Role_Vm", "Role_WindowManager", "Role_Renderer" }, vm.HostRows.Select(r => r.Label));
        Assert.Equal("300.0 MB", vm.HostRows[2].Size);
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
        // Split-APK bundles go to the install as APKs do, and App Bundles to be told why not.
        foreach (var bundle in new[] { @"C:\app.xapk", @"C:\app.apks", @"C:\app.APKM", @"C:\app.aab" })
        {
            Assert.True(vm.Accepts(new[] { bundle }), bundle);
        }
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

    private const string ClockOffer =
        "{\"provider\":\"com.example.clock/.ClockWidget\",\"package\":\"com.example.clock\",\"label\":\"Clock\"," +
        "\"preview_png\":\"iVBORw==\",\"default_width_dp\":180,\"default_height_dp\":110,\"needs_configuration\":false,\"app\":\"Clock App\"}";

    private const string NotesOffer =
        "{\"provider\":\"com.example.notes/.NoteWidget\",\"package\":\"com.example.notes\",\"label\":\"\"," +
        "\"preview_png\":\"\",\"default_width_dp\":250,\"default_height_dp\":250,\"needs_configuration\":true}";

    private const string PlacedClock =
        "{\"id\":3,\"provider\":\"com.example.clock/.ClockWidget\",\"label\":\"Clock\",\"app\":\"Clock App\"," +
        "\"placement\":{\"x\":64,\"y\":64,\"width_dp\":180,\"height_dp\":110}}";

    [Fact]
    public async Task Fr12_widgets_page_lists_the_widgets_on_offer_and_the_ones_placed()
    {
        var cli = new FakeCli().Answer("widgets list --json",
            "{\"providers\":[" + ClockOffer + "," + NotesOffer + "],\"widgets\":[" + PlacedClock + "]}");
        var vm = new WidgetsViewModel(new DsaService(cli), new KeyText());
        await vm.RefreshAsync();
        Assert.False(vm.HasError);
        Assert.Equal(2, vm.Offers.Count);
        Assert.Equal("Clock", vm.Offers[0].Label);
        Assert.Equal("180x110", vm.Offers[0].Detail);
        Assert.Equal(new byte[] { 0x89, (byte)'P', (byte)'N', (byte)'G' }, vm.Offers[0].Preview);
        // A widget without a name shows its provider, one without a preview none, and one that
        // asks to be set up says so.
        Assert.Equal("com.example.notes/.NoteWidget", vm.Offers[1].Label);
        Assert.Null(vm.Offers[1].Preview);
        Assert.Equal("250x250 · Widgets_Setup", vm.Offers[1].Detail);
        Assert.Equal(new PlacedWidgetRow(3, "Clock", "Clock App", "64,64 180x110"), Assert.Single(vm.Placed));
        // The app that offers a widget shows by its name, else by its package.
        Assert.Equal("Clock App", vm.Offers[0].App);
        Assert.Equal("com.example.notes", vm.Offers[1].App);
        Assert.False(vm.HasNoOffers);
        Assert.False(vm.HasNoPlaced);
    }

    [Fact]
    public async Task Fr12_widgets_page_places_and_removes_a_widget_and_shows_the_list_after()
    {
        var cli = new FakeCli()
            .Answer("widgets add com.example.clock/.ClockWidget --json", PlacedClock)
            .Answer("widgets remove 3 --json", "{\"removed\":3}")
            .Answer("widgets list --json", "{\"providers\":[" + ClockOffer + "],\"widgets\":[" + PlacedClock + "]}");
        var vm = new WidgetsViewModel(new DsaService(cli), new KeyText());
        await vm.AddAsync("com.example.clock/.ClockWidget");
        Assert.Equal("placed Clock", vm.Message);
        Assert.Single(vm.Placed);
        await vm.RemoveAsync(3);
        Assert.Equal("removed Clock", vm.Message);
        Assert.Equal(
            new[]
            {
                "widgets add com.example.clock/.ClockWidget --json", "widgets list --json",
                "widgets remove 3 --json", "widgets list --json",
            },
            cli.Calls);
        Assert.False(vm.IsBusy);
    }

    [Fact]
    public async Task Fr12_widgets_page_says_why_a_widget_was_not_placed()
    {
        var cli = new FakeCli()
            .Fail("widgets add com.example.clock/.ClockWidget --json",
                "dsa: the widget was not placed: the user canceled the widget's setup")
            .Answer("widgets list --json", "{\"providers\":[],\"widgets\":[]}");
        var vm = new WidgetsViewModel(new DsaService(cli), new KeyText());
        await vm.AddAsync("com.example.clock/.ClockWidget");
        Assert.True(vm.HasError);
        Assert.Contains("the user canceled the widget's setup", vm.Error);
        Assert.False(vm.HasMessage);
        await vm.RefreshAsync();
        Assert.False(vm.HasError);
        Assert.True(vm.HasNoOffers);
        Assert.True(vm.HasNoPlaced);
    }

    [Fact]
    public async Task Fr12_widgets_page_shows_a_widget_placed_or_removed_elsewhere_without_a_restart()
    {
        var cli = new FakeCli().Answer("widgets list --json",
            "{\"providers\":[" + ClockOffer + "],\"widgets\":[]}");
        var stream = new FakeLogStream();
        stream.Output.Add("{\"event\":\"widgets\",\"widgets\":[]}");
        stream.Output.Add("{\"event\":\"placed\",\"widget\":" + PlacedClock + "}");
        stream.Output.Add("not json");
        stream.Output.Add("{\"event\":\"placed\",\"widget\":" + PlacedClock.Replace("\"id\":3", "\"id\":5") + "}");
        stream.Output.Add("{\"event\":\"removed\",\"id\":3}");
        var vm = new WidgetsViewModel(new DsaService(cli), new KeyText(), stream);
        await vm.RefreshAsync();
        Assert.True(vm.HasNoPlaced);
        var changes = 0;
        vm.PropertyChanged += (_, _) => changes++;
        await vm.WatchOnceAsync(CancellationToken.None);
        Assert.Equal(new[] { "widgets watch --json" }, stream.Calls);
        // The widget placed from its window's menu or by dsa is listed, the one removed is
        // gone, and the widgets on offer are still there.
        Assert.Equal(5, Assert.Single(vm.Placed).Id);
        Assert.Single(vm.Offers);
        Assert.True(changes >= 4);
    }

    [Fact]
    public void Fr12_widgets_watch_lines_are_read_and_others_ignored()
    {
        Assert.Equal(new WidgetChange.Removed(7), DsaService.ParseWidgetChange("{\"event\":\"removed\",\"id\":7}"));
        var placed = Assert.IsType<WidgetChange.Placed>(
            DsaService.ParseWidgetChange("{\"event\":\"placed\",\"widget\":" + PlacedClock + "}"));
        Assert.Equal(new PlacedWidget(3, "com.example.clock/.ClockWidget", "Clock", "Clock App", 64, 64, 180, 110),
            placed.Widget);
        Assert.Null(DsaService.ParseWidgetChange("{\"event\":\"frame\"}"));
        Assert.Null(DsaService.ParseWidgetChange("[]"));
        Assert.Null(DsaService.ParseWidgetChange("{"));
    }
}
