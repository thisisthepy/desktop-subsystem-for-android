using System.Globalization;
using Xunit;

namespace Dsa.Settings.Core.Tests;

internal sealed class AppsText : ITextSource
{
    public string Get(string key) => key switch
    {
        "Apps_Count" => "{0} apps",
        "Apps_CountMatching" => "{0} of {1} apps",
        "Apps_Version" => "v{0}",
        "Apps_Installed" => "since {0}",
        "Apps_System" => "system",
        "Apps_Uninstalled" => "uninstalled {0}",
        "Apps_NotUninstalled" => "kept {0}: {1} [{2}]",
        "Apps_Refused_system-app" => "part of Android",
        "Apps_DesktopShortcutMade" => "on the desktop: {0}",
        "Apps_RootGroup" => "Apps_RootGroup {0}",
        _ => key,
    };
}

public class AppsViewModelTests
{
    // Notes (a user app, 2023-11-14) and Settings (a system app), as dsa apps list --json prints them.
    private const string List =
        "{\"apps\":[" +
        "{\"package\":\"com.android.settings\",\"label\":\"Settings\",\"version_name\":\"16\",\"version_code\":36," +
        "\"first_install_time_ms\":0,\"last_update_time_ms\":0,\"system_app\":true,\"app_bytes\":41943040," +
        "\"data_bytes\":0,\"cache_bytes\":0,\"total_bytes\":41943040,\"icon_png\":\"iVBORwE=\"}," +
        "{\"package\":\"com.example.notes\",\"label\":\"Notes\",\"version_name\":\"4.1\",\"version_code\":41," +
        "\"first_install_time_ms\":1700000000000,\"last_update_time_ms\":1710000000000,\"system_app\":false," +
        "\"app_bytes\":3145728,\"data_bytes\":1048576,\"cache_bytes\":0,\"total_bytes\":4194304,\"icon_png\":\"\"}]}";

    private const string AfterUninstall =
        "{\"apps\":[" +
        "{\"package\":\"com.android.settings\",\"label\":\"Settings\",\"version_name\":\"16\",\"version_code\":36," +
        "\"first_install_time_ms\":0,\"last_update_time_ms\":0,\"system_app\":true,\"app_bytes\":41943040," +
        "\"data_bytes\":0,\"cache_bytes\":0,\"total_bytes\":41943040,\"icon_png\":\"iVBORwE=\"}]}";

    public AppsViewModelTests()
    {
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
    }

    private static (AppsViewModel, FakeCli) Make(FakeCli cli) =>
        (new AppsViewModel(new AppsService(cli), new AppsText()), cli);

    [Fact]
    public async Task Fr23_the_apps_page_lists_each_app_with_its_icon_version_size_and_date()
    {
        var (vm, _) = Make(new FakeCli().Answer("apps list --json", List));
        await vm.RefreshAsync();
        Assert.False(vm.HasError, vm.Error);
        Assert.Equal(new[] { "Notes", "Settings" }, vm.Rows.Select(r => r.Label));
        var notes = vm.Rows[0];
        Assert.Equal("com.example.notes", notes.Package);
        Assert.StartsWith("com.example.notes · v4.1 · since ", notes.Detail);
        Assert.Contains("2023", notes.Detail);
        Assert.Equal("4.0 MB", notes.Size);
        Assert.True(notes.CanUninstall);
        Assert.True(notes.NoIcon);
        var settings = vm.Rows[1];
        Assert.False(settings.CanUninstall);
        Assert.True(settings.IsSystem);
        Assert.EndsWith("· system", settings.Detail);
        Assert.Equal(new byte[] { 0x89, 0x50, 0x4E, 0x47, 0x01 }, settings.Icon);
        Assert.Equal("40.0 MB", settings.Size);
        Assert.Equal("2 apps", vm.Count);
    }

    [Fact]
    public async Task Fr23_search_finds_an_app_by_name_or_package()
    {
        var (vm, _) = Make(new FakeCli().Answer("apps list --json", List));
        await vm.RefreshAsync();
        vm.Search = "note";
        Assert.Equal(new[] { "Notes" }, vm.Rows.Select(r => r.Label));
        Assert.Equal("1 of 2 apps", vm.Count);
        vm.Search = "android.set";
        Assert.Equal(new[] { "Settings" }, vm.Rows.Select(r => r.Label));
        vm.Search = "nothing";
        Assert.Empty(vm.Rows);
        Assert.True(vm.HasNoMatches);
    }

    [Fact]
    public async Task Fr22_app_info_of_a_window_opens_the_apps_page_at_the_app()
    {
        // A task window's App info starts the settings app with its app's package.
        Assert.Equal("com.example.notes",
            StartArguments.AppOf(new[] { "DsaSettings.exe", "--page", "apps", "--app", "com.example.notes" }));
        Assert.Null(StartArguments.AppOf(new[] { "DsaSettings.exe", "--page", "apps" }));
        Assert.Null(StartArguments.AppOf(new[] { "DsaSettings.exe", "--app" }));
        Assert.Null(StartArguments.AppOf(new[] { "DsaSettings.exe", "--app", "../evil" }));
        Assert.Null(StartArguments.AppOf(new[] { "DsaSettings.exe", "--app", "notes" }));

        var (vm, _) = Make(new FakeCli().Answer("apps list --json", List));
        vm.Search = "set";
        vm.Highlight("com.example.notes");
        await vm.RefreshAsync();
        // The search that would hide the app is cleared, and only its row is marked.
        Assert.Equal("", vm.Search);
        Assert.Equal("com.example.notes", vm.Highlighted);
        Assert.Equal(new[] { true, false }, vm.Rows.Select(r => r.IsHighlighted));
        Assert.Equal("com.example.notes", vm.Rows.Single(r => r.IsHighlighted).Package);

        // A search that shows the app is kept.
        vm.Search = "note";
        vm.Highlight("com.example.notes");
        Assert.Equal("note", vm.Search);
        Assert.True(vm.Rows.Single().IsHighlighted);
    }

    [Fact]
    public async Task Fr23_open_app_info_and_manage_apps_go_through_dsa()
    {
        var (vm, cli) = Make(new FakeCli()
            .Answer("apps list --json", List)
            .Answer("settings --json", "{}")
            .Answer("launch com.example.notes", "Launching com.example.notes")
            .Answer("apps settings com.example.notes --json", "{\"opened\":\"com.example.notes\"}")
            .Answer("apps settings --json", "{\"opened\":null}"));
        await vm.RefreshAsync();
        await vm.OpenAsync("com.example.notes");
        await vm.InfoAsync("com.example.notes");
        await vm.ManageAsync();
        Assert.False(vm.HasError, vm.Error);
        Assert.Equal(
            new[] { "apps list --json", "settings --json", "launch com.example.notes", "apps settings com.example.notes --json", "apps settings --json" },
            cli.Calls);
    }

    [Fact]
    public async Task Fr23_uninstalling_a_user_app_removes_it_from_the_list()
    {
        var cli = new FakeCli()
            .Answer("apps list --json", List)
            .Answer("apps uninstall com.example.notes --json",
                "{\"package\":\"com.example.notes\",\"uninstalled\":true,\"status\":\"uninstalled\",\"message\":\"Success\"}");
        var (vm, _) = Make(cli);
        await vm.RefreshAsync();
        cli.Answer("apps list --json", AfterUninstall);
        await vm.UninstallAsync("com.example.notes");
        Assert.False(vm.HasError, vm.Error);
        Assert.Equal("uninstalled Notes", vm.Message);
        Assert.Equal(new[] { "Settings" }, vm.Rows.Select(r => r.Label));
    }

    [Fact]
    public async Task Fr7_create_desktop_shortcut_puts_the_app_on_the_desktop_through_dsa()
    {
        var cli = new FakeCli()
            .Answer("apps list --json", List)
            .Answer("apps desktop-shortcut com.example.notes --json",
                "{\"package\":\"com.example.notes\",\"created\":true,\"path\":\"C:\\\\Users\\\\me\\\\Desktop\\\\Notes.lnk\"}");
        var (vm, _) = Make(cli);
        await vm.RefreshAsync();
        await vm.CreateDesktopShortcutAsync("com.example.notes");
        Assert.False(vm.HasError, vm.Error);
        Assert.Equal("on the desktop: Notes", vm.Message);
        Assert.Contains("apps desktop-shortcut com.example.notes --json", cli.Calls);
    }

    [Fact]
    public async Task Fr7_a_desktop_shortcut_that_could_not_be_made_says_why()
    {
        var (vm, _) = Make(new FakeCli()
            .Answer("apps list --json", List)
            .Fail("apps desktop-shortcut com.example.notes --json",
                "dsa: Notes has no program to start yet"));
        await vm.RefreshAsync();
        await vm.CreateDesktopShortcutAsync("com.example.notes");
        Assert.True(vm.HasError);
        Assert.Contains("Notes has no program to start yet", vm.Error);
    }

    [Fact]
    public async Task Fr23_a_refused_uninstall_says_why_in_the_users_language()
    {
        var (vm, _) = Make(new FakeCli()
            .Answer("apps list --json", List)
            .Fail("apps uninstall com.android.settings --json",
                "dsa: not uninstalled (system-app): The app is part of Android and cannot be uninstalled"));
        await vm.RefreshAsync();
        await vm.UninstallAsync("com.android.settings");
        Assert.True(vm.HasError);
        Assert.Equal("kept Settings: part of Android [The app is part of Android and cannot be uninstalled]", vm.Error);
        Assert.Equal(2, vm.Rows.Count);
    }

    // An app with partial x86_64 code, one without native code, and an app that runs as ARM
    // with nothing to go back to, as dsa apps list --json prints them.
    private static string ArmList(bool translation) =>
        "{\"apps\":[" +
        "{\"package\":\"com.example.game\",\"label\":\"Game\",\"system_app\":false,\"icon_png\":\"\"," +
        "\"primary_abi\":\"x86_64\",\"native_abis\":[\"arm64-v8a\",\"x86_64\"]," +
        $"\"run_as_arm\":{{\"on\":false,\"available\":{(translation ? "true" : "false")}," +
        $"\"reason\":\"{(translation ? "" : "translation-off")}\"}}}}," +
        "{\"package\":\"com.example.notes\",\"label\":\"Notes\",\"system_app\":false,\"icon_png\":\"\"," +
        "\"primary_abi\":\"\",\"native_abis\":[]," +
        "\"run_as_arm\":{\"on\":false,\"available\":false,\"reason\":\"no-arm-libraries\"}}," +
        "{\"package\":\"com.example.tool\",\"label\":\"Tool\",\"system_app\":false,\"icon_png\":\"\"," +
        "\"primary_abi\":\"arm64-v8a\",\"native_abis\":[\"arm64-v8a\"]," +
        "\"run_as_arm\":{\"on\":true,\"available\":false,\"reason\":\"arm-only\"}}]}";

    [Fact]
    public async Task Fr25_the_run_as_arm_switch_is_off_with_its_reason_when_it_cannot_be_changed()
    {
        var (vm, _) = Make(new FakeCli().Answer("apps list --json", ArmList(translation: false)));
        await vm.RefreshAsync();
        Assert.False(vm.HasError, vm.Error);
        var game = vm.Rows.Single(r => r.Package == "com.example.game");
        Assert.False(game.CanRunAsArm);
        Assert.Equal("Apps_RunAsArm_translation-off", game.RunAsArmTip);
        Assert.True(game.HasRunAsArmTip);
        var notes = vm.Rows.Single(r => r.Package == "com.example.notes");
        Assert.False(notes.CanRunAsArm);
        Assert.Equal("Apps_RunAsArm_no-arm-libraries", notes.RunAsArmTip);
        var tool = vm.Rows.Single(r => r.Package == "com.example.tool");
        Assert.True(tool.RunAsArm);
        Assert.False(tool.CanRunAsArm);
        Assert.Equal("Apps_RunAsArm_arm-only", tool.RunAsArmTip);
        // An older dsa that says nothing of the switch leaves it off and says so.
        Assert.Equal("Apps_RunAsArm_unknown", vm.RunAsArmTipOf(RunAsArmSwitch.Unknown));
        Assert.Null(vm.RunAsArmTipOf(new RunAsArmSwitch(false, true, "")));
    }

    [Fact]
    public async Task Fr25_turning_run_as_arm_on_keeps_the_data_or_asks_before_it_goes()
    {
        var cli = new FakeCli()
            .Answer("apps list --json", ArmList(translation: true))
            .Fail("apps run-as-arm com.example.game on --json",
                "dsa: not changed (data-would-be-erased): com.example.game: Android cannot change how the app runs and keep its data")
            .Answer("apps run-as-arm com.example.game on --erase-data --json",
                "{\"package\":\"com.example.game\",\"status\":\"done\",\"on\":true,\"message\":\"\"}");
        var (vm, _) = Make(cli);
        await vm.RefreshAsync();
        Assert.True(vm.Rows.Single(r => r.Package == "com.example.game").CanRunAsArm);

        // The user keeps the data: nothing is erased.
        var asked = new List<string>();
        await vm.SetRunAsArmAsync("com.example.game", true, label =>
        {
            asked.Add(label);
            return Task.FromResult(false);
        });
        Assert.Equal(new[] { "Game" }, asked);
        Assert.Equal("Apps_RunAsArmKept", vm.Message);
        Assert.DoesNotContain("apps run-as-arm com.example.game on --erase-data --json", cli.Calls);

        // The user lets it go: the app is installed again and its data erased.
        await vm.SetRunAsArmAsync("com.example.game", true, _ => Task.FromResult(true));
        Assert.False(vm.HasError, vm.Error);
        Assert.Contains("apps run-as-arm com.example.game on --erase-data --json", cli.Calls);
        Assert.Equal("Apps_RunAsArmOn", vm.Message);
    }

    [Fact]
    public async Task Fr25_a_switch_that_keeps_the_data_asks_nothing()
    {
        var cli = new FakeCli()
            .Answer("apps list --json", ArmList(translation: true))
            .Answer("apps run-as-arm com.example.game on --json",
                "{\"package\":\"com.example.game\",\"status\":\"done\",\"on\":true,\"message\":\"\"}")
            .Fail("apps run-as-arm com.example.notes on --json",
                "dsa: not changed (no-libraries): com.example.notes: The app's APKs on the device hold no code for that ABI");
        var (vm, _) = Make(cli);
        await vm.RefreshAsync();
        await vm.SetRunAsArmAsync("com.example.game", true, _ => throw new InvalidOperationException("asked"));
        Assert.False(vm.HasError, vm.Error);
        Assert.Equal("Apps_RunAsArmOn", vm.Message);
        await vm.SetRunAsArmAsync("com.example.notes", true, _ => throw new InvalidOperationException("asked"));
        Assert.True(vm.HasError);
        Assert.StartsWith("Apps_RunAsArmNotChanged", vm.Error);
    }

    [Fact]
    public async Task Fr23_a_service_that_cannot_be_reached_is_said_so()
    {
        var (vm, _) = Make(new FakeCli().Fail("apps list --json", "dsa: cannot reach the DSA service"));
        await vm.RefreshAsync();
        Assert.True(vm.HasError);
        Assert.StartsWith("Problem_ServiceUnreachable", vm.Error);
        Assert.Empty(vm.Rows);
    }
}
