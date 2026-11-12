using System.Globalization;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>The protected content section: its state before an install, after one and while a
/// restart is due, that its buttons do what <c>dsa drm widevine</c> does, and that it says what
/// L3 means for streaming.</summary>
public class WidevineViewModelTests
{
    private const string Settings =
        "{\"render_gpu\":\"display\",\"freeze\":false,\"program_updates\":true,\"image_updates\":true," +
        "\"program_updates_available\":true,\"image_updates_available\":false,\"version\":\"0.0.9\"}";

    private const string Package =
        "{\"apex\":\"com.google.android.widevine.apex\",\"version\":\"BE2A.250530.026.F3 (API 36, r7)\"," +
        "\"android_major\":16,\"abi\":\"x86_64\"," +
        "\"source\":\"https://dl.google.com/android/repository/sys-img/google_apis/x86_64-36_r07.zip\"," +
        "\"size\":4096,\"sha256\":\"00\",\"license\":\"android-sdk-license\"}";

    private static string Json(bool value) => value ? "true" : "false";

    private static string Status(string package, bool onDisk, bool running, bool active, bool restart) =>
        "{\"package\":" + package + ",\"installed\":" + Json(package != "null") + ",\"on_disk\":" + Json(onDisk) +
        ",\"running\":" + Json(running) + ",\"active\":" + Json(active) + ",\"restart_needed\":" + Json(restart) +
        ",\"security_level\":\"L3\",\"terms_url\":\"https://developer.android.com/studio/terms\"}";

    private static readonly string NotInstalled = Status("null", false, true, false, false);
    private static readonly string InstalledPending = Status(Package, true, true, false, true);
    private static readonly string Installed = Status(Package, true, true, true, false);
    private static readonly string RemovedPending = Status("null", false, true, true, true);

    public WidevineViewModelTests()
    {
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
    }

    private static FakeCli Cli(string status) => new FakeCli()
        .Answer("settings --json", Settings)
        .Fail("gpus --json", "dsa: no GPU list")
        .Fail("arm-translation status --json", "dsa: no ARM translation")
        .Fail("gapps status --json", "dsa: no Google Play")
        .Answer("drm widevine status --json", status);

    [Fact]
    public async Task Fr37_before_an_install_the_section_says_widevine_is_not_installed_and_offers_install()
    {
        var vm = new SettingsViewModel(new DsaService(Cli(NotInstalled)), new KeyText());
        await vm.LoadAsync();
        Assert.Equal(WidevineState.NotInstalled, vm.WidevineState);
        Assert.Equal("Widevine_NotInstalled", vm.WidevineStatus);
        Assert.True(vm.CanInstallWidevine);
        Assert.False(vm.CanRemoveWidevine);
        Assert.False(vm.WidevineRestartNeeded);
        Assert.False(vm.IsInstallingWidevine);
        // The section always says that L3 is standard definition, and where to watch in HD.
        Assert.Equal("Widevine_Quality", vm.WidevineQualityNotice);
        Assert.Equal("https://developer.android.com/studio/terms", vm.WidevineTermsUrl);
        Assert.Equal("Widevine_Notice", vm.WidevineNotice);
    }

    [Fact]
    public async Task Fr37_install_runs_dsa_drm_widevine_install_with_the_users_agreement_and_waits_for_a_restart()
    {
        var cli = Cli(NotInstalled).Answer("drm widevine install --yes --json", InstalledPending);
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        await vm.InstallWidevineAsync();
        Assert.Contains("drm widevine install --yes --json", cli.Calls);
        Assert.False(vm.HasError);
        Assert.Equal(WidevineState.RestartPending, vm.WidevineState);
        Assert.Equal("Widevine_InstalledPending", vm.WidevineStatus);
        Assert.True(vm.WidevineRestartNeeded);
        Assert.False(vm.CanInstallWidevine, "a module is installed already");
        Assert.True(vm.CanRemoveWidevine);
    }

    [Fact]
    public async Task Fr37_after_a_restart_the_section_names_the_module_and_offers_remove()
    {
        var vm = new SettingsViewModel(new DsaService(Cli(Installed)), new KeyText());
        await vm.LoadAsync();
        Assert.Equal(WidevineState.Installed, vm.WidevineState);
        Assert.Equal("Widevine_Installed", vm.WidevineStatus);
        Assert.False(vm.WidevineRestartNeeded);
        Assert.Equal(16, vm.Widevine!.Package!.AndroidMajor);
        Assert.Equal("x86_64", vm.Widevine.Package.Abi);
        Assert.True(vm.CanRemoveWidevine);
    }

    [Fact]
    public async Task Fr37_remove_runs_dsa_drm_widevine_remove_and_waits_for_a_restart()
    {
        var cli = Cli(Installed).Answer("drm widevine remove --json", RemovedPending);
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        await vm.RemoveWidevineAsync();
        Assert.Contains("drm widevine remove --json", cli.Calls);
        Assert.Equal(WidevineState.RestartPending, vm.WidevineState);
        Assert.Equal("Widevine_RemovedPending", vm.WidevineStatus);
        Assert.True(vm.CanInstallWidevine);
        Assert.False(vm.CanRemoveWidevine);
    }

    [Fact]
    public async Task Fr37_a_refused_install_shows_why_and_keeps_the_state()
    {
        var cli = Cli(NotInstalled).Fail("drm widevine install --yes --json",
            "dsa: Google's image for Android 16 on x86_64 holds no Widevine module; nothing was changed");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        await vm.InstallWidevineAsync();
        Assert.True(vm.HasError);
        Assert.Equal(WidevineState.NotInstalled, vm.WidevineState);
        Assert.False(vm.IsInstallingWidevine);
        Assert.True(vm.CanInstallWidevine);
    }
}
