using System.Globalization;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>Root access: the computer's switch on the Advanced page, which only an
/// administrator changes, and each app's level on the Apps page.</summary>
public class RootElevationViewModelTests
{
    private const string Settings =
        "{\"render_gpu\":\"display\",\"version\":\"0.1.10\",\"log_dir\":\"C:\\\\ProgramData\\\\DSA\\\\logs\"," +
        "\"root_elevation\":false}";

    private const string On = "{\"render_gpu\":\"display\",\"version\":\"0.1.10\",\"log_dir\":\"C:\\\\ProgramData\\\\DSA\\\\logs\"," +
        "\"root_elevation\":true}";

    // A backup app at "ask", a notes app at "off" and Settings at "remember", as dsa apps list
    // --json prints them while root elevation is on.
    private const string Apps =
        "{\"apps\":[" +
        "{\"package\":\"com.example.backup\",\"label\":\"Backup\",\"root\":\"ask\"}," +
        "{\"package\":\"com.example.notes\",\"label\":\"Notes\",\"root\":\"off\"}," +
        "{\"package\":\"com.android.settings\",\"label\":\"Settings\",\"system_app\":true,\"root\":\"remember\"}]}";

    private const string AppsWithFeatureOff =
        "{\"apps\":[" +
        "{\"package\":\"com.example.backup\",\"label\":\"Backup\",\"root\":\"unavailable\"}," +
        "{\"package\":\"com.example.notes\",\"label\":\"Notes\",\"root\":\"unavailable\"}]}";

    public RootElevationViewModelTests()
    {
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
    }

    private static FakeCli Cli(string settings) => new FakeCli()
        .Answer("settings --json", settings)
        .Fail("gpus --json", "dsa: no GPU list");

    [Fact]
    public async Task Fr35_root_elevation_is_off_by_default_and_only_an_administrator_changes_it()
    {
        var cli = Cli("{\"render_gpu\":\"display\",\"version\":\"0.1.9\"}");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        // A service from before the setting says nothing of it: off.
        Assert.False(vm.RootElevation);

        // A user who is not an administrator sees why the switch does not move.
        Assert.False(vm.CanChangeRootElevation);
        Assert.True(vm.RootElevationNeedsAdministrator);

        vm.IsAdministrator = true;
        Assert.True(vm.CanChangeRootElevation);
        Assert.False(vm.RootElevationNeedsAdministrator);
    }

    [Fact]
    public async Task Fr35_an_administrator_turns_root_elevation_on_and_the_page_says_it_applies_from_the_next_start()
    {
        var cli = Cli(Settings).Answer("settings set root-elevation on --json", On);
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText()) { IsAdministrator = true };
        await vm.LoadAsync();
        Assert.False(vm.RootElevation);
        Assert.False(vm.RootElevationRestartNeeded);

        await vm.SetRootElevationAsync(true);
        Assert.False(vm.HasError, vm.Error);
        Assert.True(vm.RootElevation);
        Assert.Contains("settings set root-elevation on --json", cli.Calls);
        Assert.True(vm.RootElevationRestartNeeded);

        // Setting it to what it is sends nothing.
        var calls = cli.Calls.Count;
        await vm.SetRootElevationAsync(true);
        Assert.Equal(calls, cli.Calls.Count);
    }

    [Fact]
    public async Task Fr35_a_refused_change_leaves_the_switch_as_the_service_keeps_it()
    {
        var cli = Cli(Settings).Fail("settings set root-elevation on --json",
            "dsa: only an administrator may change root elevation");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText()) { IsAdministrator = true };
        await vm.LoadAsync();
        await vm.SetRootElevationAsync(true);
        Assert.True(vm.HasError);
        Assert.Contains("only an administrator", vm.Error);
        Assert.False(vm.RootElevation);
        Assert.False(vm.RootElevationRestartNeeded);
    }

    [Fact]
    public async Task Fr35_the_page_says_where_the_requests_are_recorded()
    {
        var vm = new SettingsViewModel(new DsaService(Cli(Settings)), new KeyText());
        await vm.LoadAsync();
        Assert.Equal("RootElevation_LogAt", vm.RootElevationLog);

        var withoutDir = new SettingsViewModel(
            new DsaService(Cli("{\"render_gpu\":\"display\",\"version\":\"0.1.10\"}")), new KeyText());
        await withoutDir.LoadAsync();
        Assert.Equal("RootElevation_LogUnknown", withoutDir.RootElevationLog);
    }

    private static (AppsViewModel, FakeCli) MakeApps(FakeCli cli) =>
        (new AppsViewModel(new AppsService(cli), new AppsText()), cli);

    [Fact]
    public async Task Fr35_adb_shell_asks_for_root_like_an_app()
    {
        var cli = new FakeCli().Answer("apps list --json", Apps)
            .Answer("settings --json", "{\"adb_shell_root\":\"ask\"}")
            .Answer("apps root com.android.shell off --json", "{\"level\":\"off\"}");
        var (vm, _) = MakeApps(cli);
        await vm.RefreshAsync();
        Assert.True(vm.ShowAdbShellRoot);
        Assert.Equal((int)RootLevel.Ask, vm.AdbShellRootIndex);
        cli.Answer("settings --json", "{\"adb_shell_root\":\"off\"}");
        await vm.SetRootAsync("com.android.shell", (int)RootLevel.Off);
        Assert.Contains("apps root com.android.shell off --json", cli.Calls);
        Assert.Equal((int)RootLevel.Off, vm.AdbShellRootIndex);
        cli.Answer("settings --json", "{\"adb_shell_root\":\"\"}");
        await vm.RefreshAsync();
        Assert.False(vm.ShowAdbShellRoot);
    }

    [Fact]
    public async Task Fr35_each_app_shows_its_root_level_off_ask_or_remember()
    {
        var (vm, _) = MakeApps(new FakeCli().Answer("apps list --json", Apps));
        await vm.RefreshAsync();
        Assert.False(vm.HasError, vm.Error);
        Assert.False(vm.RootUnavailable);
        Assert.Equal(new[] { RootLevel.Off, RootLevel.Ask, RootLevel.Remember }, vm.RootChoices.Select(c => c.Level));
        Assert.Equal(new[] { "Apps_Root_off", "Apps_Root_ask", "Apps_Root_remember" }, vm.RootChoices.Select(c => c.Label));

        var backup = vm.Rows.Single(r => r.Package == "com.example.backup");
        Assert.Equal((int)RootLevel.Ask, backup.RootIndex);
        Assert.True(backup.CanChangeRoot);
        Assert.Null(backup.RootTip);
        Assert.Equal((int)RootLevel.Off, vm.Rows.Single(r => r.Package == "com.example.notes").RootIndex);
        Assert.Equal((int)RootLevel.Remember, vm.Rows.Single(r => r.Package == "com.android.settings").RootIndex);
    }

    [Fact]
    public async Task Fr35_no_app_level_can_be_chosen_while_root_elevation_is_off_for_the_pc()
    {
        var (vm, _) = MakeApps(new FakeCli().Answer("apps list --json", AppsWithFeatureOff));
        await vm.RefreshAsync();
        Assert.True(vm.RootUnavailable);
        Assert.All(vm.Rows, row =>
        {
            Assert.False(row.CanChangeRoot);
            Assert.Equal(-1, row.RootIndex);
            Assert.Equal("Apps_RootTip_unavailable", row.RootTip);
        });

        // A service from before root levels says nothing; the list cannot be used either.
        var (older, _) = MakeApps(new FakeCli().Answer("apps list --json",
            "{\"apps\":[{\"package\":\"com.example.notes\",\"label\":\"Notes\"}]}"));
        await older.RefreshAsync();
        Assert.False(older.RootUnavailable);
        Assert.False(older.Rows.Single().CanChangeRoot);
        Assert.Equal("Apps_RootTip_unknown", older.Rows.Single().RootTip);
    }

    [Fact]
    public async Task Fr35_apps_sharing_a_user_id_are_approved_as_one_group()
    {
        const string shared =
            "{\"apps\":[" +
            "{\"package\":\"com.example.zeta\",\"label\":\"Zeta\",\"root\":\"ask\",\"shared_user_id\":\"com.example.shared\"}," +
            "{\"package\":\"com.example.middle\",\"label\":\"Middle\",\"root\":\"off\"}," +
            "{\"package\":\"com.example.alpha\",\"label\":\"Alpha\",\"root\":\"ask\",\"shared_user_id\":\"com.example.shared\"}]}";
        var (vm, _) = MakeApps(new FakeCli().Answer("apps list --json", shared));
        await vm.RefreshAsync();
        Assert.False(vm.HasError, vm.Error);

        // The group is listed together, in the place of its first app by name.
        Assert.Equal(new[] { "Alpha", "Zeta", "Middle" }, vm.Rows.Select(r => r.Label));
        // Each app of the group names the others and says root is one setting for them.
        var zeta = vm.Rows.Single(r => r.Package == "com.example.zeta");
        Assert.True(zeta.HasRootGroupNote);
        Assert.Contains("Alpha", zeta.RootGroupNote);
        Assert.StartsWith("Apps_RootGroup", zeta.RootGroupNote);
        Assert.False(vm.Rows.Single(r => r.Package == "com.example.middle").HasRootGroupNote);

        // A search that finds one app of the group shows the whole group.
        vm.Search = "zeta";
        Assert.Equal(new[] { "Alpha", "Zeta" }, vm.Rows.Select(r => r.Label));
    }

    [Fact]
    public async Task Fr35_choosing_a_level_goes_through_dsa_apps_root()
    {
        var after = Apps.Replace("\"label\":\"Notes\",\"root\":\"off\"", "\"label\":\"Notes\",\"root\":\"ask\"");
        var cli = new FakeCli()
            .Answer("apps list --json", Apps)
            .Answer("apps root com.example.notes ask --json", "{\"package\":\"com.example.notes\",\"level\":\"ask\"}");
        var (vm, _) = MakeApps(cli);
        await vm.RefreshAsync();
        cli.Answer("apps list --json", after);

        await vm.SetRootAsync("com.example.notes", (int)RootLevel.Ask);
        Assert.False(vm.HasError, vm.Error);
        Assert.Contains("apps root com.example.notes ask --json", cli.Calls);
        Assert.Equal("Apps_RootSet_ask", vm.Message);
        Assert.Equal((int)RootLevel.Ask, vm.Rows.Single(r => r.Package == "com.example.notes").RootIndex);

        // Choosing the level the app has sends nothing.
        var calls = cli.Calls.Count;
        await vm.SetRootAsync("com.example.notes", (int)RootLevel.Ask);
        Assert.Equal(calls, cli.Calls.Count);
    }

    [Fact]
    public async Task Fr35_remember_without_an_administrators_approval_is_refused_with_the_services_words()
    {
        var cli = new FakeCli()
            .Answer("apps list --json", Apps)
            .Fail("apps root com.example.backup remember --json",
                "dsa: not changed: com.example.backup: letting an app keep root without asking needs an administrator");
        var (vm, _) = MakeApps(cli);
        await vm.RefreshAsync();

        await vm.SetRootAsync("com.example.backup", (int)RootLevel.Remember);
        Assert.True(vm.HasError);
        Assert.Equal("Apps_RootNotChanged", vm.Error);
        Assert.False(vm.HasMessage);
        // The row keeps the level the service has.
        Assert.Equal((int)RootLevel.Ask, vm.Rows.Single(r => r.Package == "com.example.backup").RootIndex);
    }

    [Fact]
    public async Task Fr35_the_refusal_names_the_app_and_says_why()
    {
        var cli = new FakeCli()
            .Answer("apps list --json", Apps)
            .Fail("apps root com.example.backup remember --json",
                "dsa: not changed: com.example.backup: letting an app keep root without asking needs an administrator");
        var vm = new AppsViewModel(new AppsService(cli), new RootText());
        await vm.RefreshAsync();
        await vm.SetRootAsync("com.example.backup", (int)RootLevel.Remember);
        Assert.Equal("Backup kept: letting an app keep root without asking needs an administrator", vm.Error);
    }

    private sealed class RootText : ITextSource
    {
        public string Get(string key) => key == "Apps_RootNotChanged" ? "{0} kept: {1}" : key;
    }
}
