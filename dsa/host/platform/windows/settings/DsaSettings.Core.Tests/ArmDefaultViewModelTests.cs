using System.Globalization;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>The default ARM translator in the ARM apps section: the one an x86_64 image
/// carries is used from the first start, a package the user installs replaces it, and the
/// section goes back to it through <c>dsa arm-translation use-default</c>. An image without one
/// shows nothing of it.</summary>
public class ArmDefaultViewModelTests
{
    private const string Settings =
        "{\"render_gpu\":\"display\",\"freeze\":false,\"program_updates\":true,\"image_updates\":true," +
        "\"program_updates_available\":true,\"image_updates_available\":false,\"version\":\"0.0.9\"}";

    private const string Digitalis =
        "{\"name\":\"Digitalis\",\"version\":\"17-abefbe2b1600\",\"library\":\"libberberis_arm64.so\"," +
        "\"abis\":[\"arm64-v8a\"],\"license\":\"Apache-2.0\"}";

    private const string Mine =
        "{\"name\":\"Translator\",\"version\":\"1.0\",\"library\":\"libtranslator.so\"," +
        "\"abis\":[\"arm64-v8a\"],\"license\":\"Apache-2.0\"}";

    private const string Google =
        "{\"name\":\"Google ARM translator\",\"version\":\"16\",\"library\":\"libndk_translation.so\"," +
        "\"abis\":[\"arm64-v8a\"],\"license\":\"Android SDK License\"}";

    private const string UseDefault = "arm-translation use-default --json";

    /// <summary>What <c>dsa arm-translation --json</c> says, with <paramref name="source"/> naming
    /// where the translator in use comes from ("" for none).</summary>
    private static string Status(string package, string @default, string source, bool enabled = true)
    {
        var used = source == "default" ? @default : package;
        var translator = source.Length == 0
            ? "null"
            : "{\"source\":\"" + source + "\"," + used.Substring(1);
        return "{\"google_fetch_available\":true,\"package\":" + package + ",\"default\":" + @default +
            ",\"translator\":" + translator + ",\"enabled\":" + (enabled ? "true" : "false") +
            ",\"supported\":" + (source.Length > 0 ? "true" : "false") +
            ",\"android\":{\"installed\":" + (package == "null" ? "false" : "true") +
            ",\"active\":true,\"abilist\":\"x86_64,arm64-v8a\"},\"restart_needed\":false}";
    }

    public ArmDefaultViewModelTests()
    {
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
    }

    private static FakeCli Cli(string status) => new FakeCli()
        .Answer("settings --json", Settings)
        .Fail("gpus --json", "dsa: no GPU list")
        .Answer("arm-translation status --json", status);

    [Fact]
    public async Task Fr25_the_default_translator_is_used_from_the_first_start()
    {
        var vm = new SettingsViewModel(new DsaService(Cli(Status("null", Digitalis, "default"))), new FormatText());
        await vm.LoadAsync();
        Assert.True(vm.ArmSupported);
        Assert.Equal("default Digitalis 17-abefbe2b1600 Apache-2.0", vm.ArmStatus);
        Assert.True(vm.HasArmDefault);
        Assert.False(vm.ShowsArmRemove, "removing a package is going back to the default");
        Assert.Equal("about Digitalis 17-abefbe2b1600 Apache-2.0", vm.ArmDefaultDescription);
        Assert.False(vm.CanUseDefaultArmTranslator, "already in use");
    }

    [Fact]
    public async Task Fr25_a_package_or_googles_translator_replaces_the_default_until_the_user_goes_back()
    {
        var cli = Cli(Status(Mine, Digitalis, "user"))
            .Answer(UseDefault, Status("null", Digitalis, "default"));
        var vm = new SettingsViewModel(new DsaService(cli), new FormatText());
        await vm.LoadAsync();
        Assert.Equal("user Translator 1.0 Apache-2.0", vm.ArmStatus);
        Assert.True(vm.CanUseDefaultArmTranslator);
        Assert.DoesNotContain(UseDefault, cli.Calls);

        await vm.UseDefaultArmTranslatorAsync();
        Assert.Contains(UseDefault, cli.Calls);
        Assert.False(vm.HasError);
        Assert.False(vm.HasArmPackage);
        Assert.Equal("default Digitalis 17-abefbe2b1600 Apache-2.0", vm.ArmStatus);
        Assert.False(vm.CanUseDefaultArmTranslator);

        var google = new SettingsViewModel(new DsaService(Cli(Status(Google, Digitalis, "google"))), new FormatText());
        await google.LoadAsync();
        Assert.Equal("user Google ARM translator 16 Android SDK License", google.ArmStatus);
        Assert.True(google.CanUseDefaultArmTranslator);
    }

    [Fact]
    public async Task Fr25_turned_off_the_section_names_the_default_translator_it_does_not_use()
    {
        var vm = new SettingsViewModel(new DsaService(Cli(Status("null", Digitalis, "", enabled: false))),
            new FormatText());
        await vm.LoadAsync();
        Assert.False(vm.ArmSupported);
        Assert.Equal("off Digitalis 17-abefbe2b1600", vm.ArmStatus);
    }

    [Fact]
    public async Task Fr25_an_image_without_a_default_translator_shows_nothing_of_it()
    {
        // Every arm64 image, and an x86_64 image made without one.
        var vm = new SettingsViewModel(new DsaService(Cli(Status("null", "null", ""))), new FormatText());
        await vm.LoadAsync();
        Assert.False(vm.HasArmDefault);
        Assert.True(vm.ShowsArmRemove);
        Assert.False(vm.CanUseDefaultArmTranslator);
        Assert.Equal("", vm.ArmDefaultDescription);
        Assert.Equal("Arm_NotSupported", vm.ArmStatus);
    }

    [Fact]
    public async Task Fr25_a_refused_switch_back_leaves_the_section_as_it_was()
    {
        var cli = Cli(Status(Mine, Digitalis, "user")).Fail(UseDefault, "dsa: Android is Starting; try again");
        var vm = new SettingsViewModel(new DsaService(cli), new FormatText());
        await vm.LoadAsync();
        await vm.UseDefaultArmTranslatorAsync();
        Assert.True(vm.HasError);
        Assert.True(vm.HasArmPackage);
        Assert.Equal("user Translator 1.0 Apache-2.0", vm.ArmStatus);
        Assert.True(vm.CanUseDefaultArmTranslator, "it can be tried again");
    }

    /// <summary>The section's texts with their places filled, so a test sees which one shows
    /// and with what.</summary>
    private sealed class FormatText : ITextSource
    {
        public string Get(string key) => key switch
        {
            "Arm_SupportedDefault" => "default {0} {1} {2}",
            "Arm_Supported" => "user {0} {1} {2}",
            "Arm_DefaultTurnedOff" => "off {0} {1}",
            "Arm_DefaultDescription" => "about {0} {1} {2}",
            _ => key,
        };
    }
}
