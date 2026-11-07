using System.Globalization;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>The Google Play section: its state before an install, after one and while a
/// restart is due, and that its buttons do what <c>dsa gapps</c> does.</summary>
public class GappsViewModelTests
{
    private const string Settings =
        "{\"render_gpu\":\"display\",\"freeze\":false,\"program_updates\":true,\"image_updates\":true," +
        "\"program_updates_available\":true,\"image_updates_available\":false,\"version\":\"0.0.9\"}";

    private const string Package =
        "{\"variant\":\"core\",\"version\":\"20261001\",\"android_version\":\"16.0\",\"sdk\":36," +
        "\"arch\":\"x86_64\",\"file\":\"LiteGapps-core-x86_64-16.0-20261001-official.zip\"," +
        "\"source\":\"https://sourceforge.net/projects/litegapps/files/litegapps/x86_64/36/core/2026-10-01/" +
        "LiteGapps-core-x86_64-16.0-20261001-official.zip/download\",\"md5\":\"ea9945a6d63849fa9e51782edfe613a4\"}";

    private static string Json(bool value) => value ? "true" : "false";

    private static string Android(bool installed, bool active, string deviceId = "") =>
        "{\"installed\":" + Json(installed) + ",\"active\":" + Json(active) +
        ",\"sdk\":36,\"abi\":\"x86_64\",\"abilist\":\"x86_64,arm64-v8a\",\"device_id\":\"" + deviceId + "\"}";

    private static string Status(string package, string android, bool restart) =>
        "{\"package\":" + package + ",\"installed\":" + Json(package != "null") + ",\"android\":" + android +
        ",\"restart_needed\":" + Json(restart) + ",\"terms_url\":\"https://policies.google.com/terms\"," +
        "\"registration_url\":\"https://www.google.com/android/uncertified/\"}";

    private static readonly string NotInstalled = Status("null", Android(false, false), false);
    private static readonly string InstalledPending = Status(Package, Android(true, false), true);
    private static readonly string Installed = Status(Package, Android(true, true, "3987654321012345678"), false);
    private static readonly string RemovedPending = Status("null", Android(false, true), true);

    public GappsViewModelTests()
    {
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
    }

    private static FakeCli Cli(string status) => new FakeCli()
        .Answer("settings --json", Settings)
        .Fail("gpus --json", "dsa: no GPU list")
        .Fail("arm-translation status --json", "dsa: no ARM translation")
        .Answer("gapps status --json", status);

    [Fact]
    public async Task Fr32_before_an_install_the_section_says_google_play_is_not_installed_and_offers_install()
    {
        var vm = new SettingsViewModel(new DsaService(Cli(NotInstalled)), new KeyText());
        await vm.LoadAsync();
        Assert.Equal(GappsState.NotInstalled, vm.GappsState);
        Assert.Equal("Gapps_NotInstalled", vm.GappsStatus);
        Assert.False(vm.HasGappsSource);
        Assert.True(vm.CanInstallGapps);
        Assert.False(vm.CanRemoveGapps);
        Assert.False(vm.GappsRestartNeeded);
        Assert.False(vm.IsInstallingGapps);
        Assert.False(vm.HasGappsDeviceId);
        Assert.True(vm.ShowsGappsDeviceIdHint);
        Assert.Equal("Gapps_NoDeviceId", vm.GappsDeviceIdText);
        Assert.Equal("https://policies.google.com/terms", vm.GappsTermsUrl);
        Assert.Equal("https://www.google.com/android/uncertified/", vm.GappsRegistrationUrl);
    }

    [Fact]
    public async Task Fr32_install_runs_dsa_gapps_install_and_waits_for_a_restart()
    {
        var cli = Cli(NotInstalled).Answer("gapps install --json", InstalledPending);
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        await vm.InstallGappsAsync();
        Assert.Contains("gapps install --json", cli.Calls);
        Assert.False(vm.HasError);
        Assert.Equal(GappsState.RestartPending, vm.GappsState);
        Assert.Equal("Gapps_InstalledPending", vm.GappsStatus);
        Assert.True(vm.GappsRestartNeeded);
        Assert.True(vm.HasGappsSource);
        Assert.Equal("Gapps_Source", vm.GappsSource);
        Assert.False(vm.CanInstallGapps, "a package is installed already");
        Assert.True(vm.CanRemoveGapps);
        Assert.False(vm.IsInstallingGapps, "the install has ended");
    }

    [Fact]
    public async Task Fr32_after_a_restart_the_section_shows_the_package_and_the_android_id()
    {
        var vm = new SettingsViewModel(new DsaService(Cli(Installed)), new KeyText());
        await vm.LoadAsync();
        Assert.Equal(GappsState.Installed, vm.GappsState);
        Assert.Equal("Gapps_Installed", vm.GappsStatus);
        Assert.False(vm.GappsRestartNeeded);
        var package = vm.Gapps!.Package!;
        Assert.Equal("20261001", package.Version);
        Assert.Equal("16.0", package.AndroidVersion);
        Assert.Equal("x86_64", package.Arch);
        Assert.True(vm.HasGappsDeviceId);
        Assert.False(vm.ShowsGappsDeviceIdHint);
        Assert.Equal("3987654321012345678", vm.GappsDeviceId);
        Assert.False(vm.CanInstallGapps);
        Assert.True(vm.CanRemoveGapps);
    }

    [Fact]
    public async Task Fr32_the_status_names_the_package_in_the_users_words()
    {
        var vm = new SettingsViewModel(new DsaService(Cli(Installed)), new FormatText());
        await vm.LoadAsync();
        Assert.Equal("installed 20261001 16.0 x86_64", vm.GappsStatus);
        Assert.Equal("from LiteGapps-core-x86_64-16.0-20261001-official.zip", vm.GappsSource);
        Assert.Equal("id 3987654321012345678", vm.GappsDeviceIdText);
    }

    [Fact]
    public async Task Fr32_remove_runs_dsa_gapps_remove_and_waits_for_a_restart_to_take_the_apps_away()
    {
        var cli = Cli(Installed).Answer("gapps remove --json", RemovedPending);
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        await vm.RemoveGappsAsync();
        Assert.Contains("gapps remove --json", cli.Calls);
        Assert.Equal(GappsState.RestartPending, vm.GappsState);
        Assert.Equal("Gapps_RemovedPending", vm.GappsStatus);
        Assert.True(vm.GappsRestartNeeded);
        Assert.False(vm.HasGappsPackage);
        Assert.True(vm.CanInstallGapps);
        Assert.False(vm.CanRemoveGapps);
    }

    [Fact]
    public async Task Fr32_a_failed_install_shows_why_and_leaves_the_section_as_it_was()
    {
        var cli = Cli(NotInstalled)
            .Fail("gapps install --json", "dsa: the package is for arm64, but Android here is x86_64");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        await vm.InstallGappsAsync();
        Assert.True(vm.HasError);
        Assert.Equal(GappsState.NotInstalled, vm.GappsState);
        Assert.True(vm.CanInstallGapps);
        Assert.False(vm.IsInstallingGapps);
    }

    [Fact]
    public async Task Fr32_the_buttons_wait_while_an_install_runs()
    {
        var install = new TaskCompletionSource<CliResult>();
        var cli = new GatedCli(Cli(NotInstalled), "gapps install --json", install.Task);
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        var running = vm.InstallGappsAsync();
        Assert.True(vm.IsInstallingGapps);
        Assert.False(vm.CanInstallGapps);
        Assert.False(vm.CanRemoveGapps);
        Assert.False(vm.CanChange);
        install.SetResult(new CliResult(0, InstalledPending, ""));
        await running;
        Assert.False(vm.IsInstallingGapps);
        Assert.True(vm.CanRemoveGapps);
    }

    [Fact]
    public async Task Fr32_a_service_that_cannot_say_shows_google_play_as_not_installed()
    {
        var cli = new FakeCli()
            .Answer("settings --json", Settings)
            .Fail("gpus --json", "dsa: no GPU list")
            .Fail("arm-translation status --json", "dsa: no ARM translation")
            .Fail("gapps status --json", "dsa: unexpected \"gapps\"");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.False(vm.HasError);
        Assert.Equal(GappsState.NotInstalled, vm.GappsState);
        Assert.Equal("Gapps_NotInstalled", vm.GappsStatus);
    }

    [Fact]
    public void Fr32_only_https_pages_from_the_service_are_opened()
    {
        using var document = System.Text.Json.JsonDocument.Parse(
            "{\"package\":null,\"terms_url\":\"file:///C:/Windows/notepad.exe\"}");
        var info = DsaService.ReadGapps(document.RootElement);
        Assert.Equal(DsaService.DefaultGappsTermsUrl, info.TermsUrl);
        Assert.Equal(DsaService.DefaultGappsRegistrationUrl, info.RegistrationUrl);
        Assert.Null(info.Package);
        Assert.Null(info.Android);
        Assert.False(info.RestartNeeded);
    }

    /// <summary>Answers as <see cref="FakeCli"/>, except one command line, whose answer comes
    /// when the test releases it.</summary>
    private sealed class GatedCli : IDsaCli
    {
        private readonly FakeCli _inner;
        private readonly string _line;
        private readonly Task<CliResult> _answer;

        public GatedCli(FakeCli inner, string line, Task<CliResult> answer)
        {
            _inner = inner;
            _line = line;
            _answer = answer;
        }

        public Task<CliResult> RunAsync(IReadOnlyList<string> args, CancellationToken cancel = default) =>
            string.Join(" ", args) == _line ? _answer : _inner.RunAsync(args, cancel);
    }

    /// <summary>Formats that show which values went in, in order.</summary>
    private sealed class FormatText : ITextSource
    {
        public string Get(string key) => key switch
        {
            "Gapps_Installed" => "installed {0} {1} {2}",
            "Gapps_Source" => "from {0}",
            "Gapps_DeviceId" => "id {0}",
            _ => key,
        };
    }
}
