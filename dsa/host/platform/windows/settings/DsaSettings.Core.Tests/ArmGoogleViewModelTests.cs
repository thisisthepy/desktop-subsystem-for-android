using System.Globalization;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>Google's ARM translator in the ARM apps section: offered only where dsa can fetch
/// it, fetched with the user's agreement through <c>dsa arm-translation fetch google</c>, and a
/// refusal that leaves the section as it was.</summary>
public class ArmGoogleViewModelTests
{
    private const string Settings =
        "{\"render_gpu\":\"display\",\"freeze\":false,\"program_updates\":true,\"image_updates\":true," +
        "\"program_updates_available\":true,\"image_updates_available\":false,\"version\":\"0.0.9\"}";

    private static string Status(bool available, string package, bool installed, bool restart) =>
        "{\"google_fetch_available\":" + (available ? "true" : "false") + ",\"package\":" + package +
        ",\"enabled\":true,\"supported\":" + (package == "null" ? "false" : "true") +
        ",\"android\":{\"installed\":" + (installed ? "true" : "false") +
        ",\"active\":false,\"abilist\":\"x86_64\"},\"restart_needed\":" + (restart ? "true" : "false") + "}";

    private const string Google =
        "{\"name\":\"Google ARM translator\",\"version\":\"BE2A.250530.026.F3 (API 36, r7)\"," +
        "\"library\":\"libndk_translation.so\",\"abis\":[\"arm64-v8a\"],\"license\":\"Android SDK License\"}";

    private const string Fetch = "arm-translation fetch google --yes --json";

    public ArmGoogleViewModelTests()
    {
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
    }

    private static FakeCli Cli(bool available) => new FakeCli()
        .Answer("settings --json", Settings)
        .Fail("gpus --json", "dsa: no GPU list")
        .Answer("arm-translation status --json", Status(available, "null", false, false));

    [Fact]
    public async Task Fr25_googles_translator_is_fetched_and_installed_once_the_user_agrees()
    {
        var cli = Cli(available: true).Answer(Fetch, Status(true, Google, true, true));
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.True(vm.ShowsGoogleArm);
        Assert.True(vm.CanFetchGoogleArm);
        Assert.False(vm.IsFetchingGoogleArm);
        Assert.Equal("Arm_NotSupported", vm.ArmStatus);
        Assert.Equal("https://developer.android.com/studio/terms", vm.GoogleArmTermsUrl);
        Assert.DoesNotContain(Fetch, cli.Calls);

        await vm.FetchGoogleArmTranslatorAsync();
        Assert.Contains(Fetch, cli.Calls);
        Assert.False(vm.HasError);
        Assert.False(vm.IsFetchingGoogleArm);
        Assert.True(vm.ArmSupported);
        Assert.True(vm.HasArmPackage);
        Assert.Equal("Arm_Supported", vm.ArmStatus);
        Assert.True(vm.ArmRestartNeeded, "it works from the next start");
        Assert.True(vm.CanRemoveArmTranslation, "it is removed like any other package");
    }

    [Fact]
    public async Task Fr25_the_notice_names_the_source_and_googles_terms()
    {
        var vm = new SettingsViewModel(new DsaService(Cli(available: true)), new NoticeText());
        await vm.LoadAsync();
        Assert.Contains("https://dl.google.com/android/repository/", vm.GoogleArmNotice);
        Assert.Contains("https://developer.android.com/studio/terms", vm.GoogleArmNotice);
    }

    [Fact]
    public async Task Fr25_googles_translator_is_not_offered_where_android_runs_arm_apps_itself()
    {
        var cli = Cli(available: false);
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.False(vm.ShowsGoogleArm);
        Assert.False(vm.CanFetchGoogleArm);
    }

    [Fact]
    public async Task Fr25_a_service_that_cannot_say_offers_no_google_translator()
    {
        var cli = new FakeCli()
            .Answer("settings --json", Settings)
            .Fail("gpus --json", "dsa: no GPU list")
            .Fail("arm-translation status --json", "dsa: unexpected \"arm-translation\"");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.False(vm.ShowsGoogleArm);
    }

    [Fact]
    public async Task Fr25_a_refused_download_leaves_the_section_as_it_was()
    {
        var cli = Cli(available: true).Fail(Fetch,
            "dsa: the download of https://dl.google.com/x.zip has the SHA-1 0123, but Google's list says 4567; " +
            "nothing was changed");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        await vm.FetchGoogleArmTranslatorAsync();
        Assert.True(vm.HasError);
        Assert.False(vm.HasArmPackage);
        Assert.Equal("Arm_NotSupported", vm.ArmStatus);
        Assert.False(vm.IsFetchingGoogleArm);
        Assert.True(vm.CanFetchGoogleArm, "it can be tried again");
    }

    /// <summary>The notice as the app words it, with its two places for addresses.</summary>
    private sealed class NoticeText : ITextSource
    {
        public string Get(string key) => key == "ArmGoogle_Notice" ? "from {0} under {1}" : key;
    }
}
