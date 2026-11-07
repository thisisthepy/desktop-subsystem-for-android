using System.Globalization;
using System.Xml.Linq;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>The Advanced page's automatic adb connection: its switch, on by default, and the
/// state the service reports, in both languages.</summary>
public class AdbAutoConnectTests
{
    private const string Off =
        "{\"render_gpu\":\"display\",\"freeze\":false,\"program_updates\":true,\"image_updates\":true," +
        "\"program_updates_available\":true,\"image_updates_available\":false,\"version\":\"0.0.9\"," +
        "\"developer_mode\":false,\"adb_port\":58526,\"adb_auto_connect\":true,\"adb_auto_connect_state\":\"off\"}";

    public AdbAutoConnectTests()
    {
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
    }

    [Fact]
    public async Task Fr20_auto_connect_is_on_by_default_and_the_switch_turns_it_off()
    {
        var cli = new FakeCli()
            .Answer("settings --json", Off)
            .Fail("gpus --json", "dsa: no GPU list")
            .Answer("settings set adb-auto-connect off --json", Off.Replace("\"adb_auto_connect\":true", "\"adb_auto_connect\":false"))
            .Answer("settings set adb-auto-connect on --json", Off);
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.True(vm.AdbAutoConnect);
        Assert.Equal(AdbAutoConnectState.Off, vm.AdbAutoConnectState);
        Assert.Equal("AdbAutoConnect_Off", vm.AdbAutoConnectStateText);

        await vm.SetAdbAutoConnectAsync(false);
        Assert.False(vm.AdbAutoConnect);
        await vm.SetAdbAutoConnectAsync(true);
        Assert.True(vm.AdbAutoConnect);
        Assert.Contains("settings set adb-auto-connect off --json", cli.Calls);
        Assert.Contains("settings set adb-auto-connect on --json", cli.Calls);
    }

    [Fact]
    public async Task Fr20_a_service_that_predates_auto_connect_reads_as_on_and_off()
    {
        var old = Off.Replace(",\"adb_auto_connect\":true,\"adb_auto_connect_state\":\"off\"", "");
        var cli = new FakeCli().Answer("settings --json", old).Fail("gpus --json", "dsa: no GPU list");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.True(vm.AdbAutoConnect);
        Assert.Equal(AdbAutoConnectState.Off, vm.AdbAutoConnectState);
    }

    [Theory]
    [InlineData("off", AdbAutoConnectState.Off)]
    [InlineData("waiting-for-server", AdbAutoConnectState.WaitingForServer)]
    [InlineData("waiting-for-android", AdbAutoConnectState.WaitingForAndroid)]
    [InlineData("attached", AdbAutoConnectState.Attached)]
    [InlineData("detached", AdbAutoConnectState.Detached)]
    [InlineData("something-new", AdbAutoConnectState.Off)]
    [InlineData(null, AdbAutoConnectState.Off)]
    public void Fr20_each_state_name_is_read(string? name, AdbAutoConnectState state) =>
        Assert.Equal(state, DsaService.ReadAdbAutoConnectState(name));

    [Fact]
    public async Task Fr20_the_state_follows_the_service_while_developer_mode_is_on()
    {
        var on = Off.Replace("\"developer_mode\":false", "\"developer_mode\":true")
            .Replace("\"adb_auto_connect_state\":\"off\"", "\"adb_auto_connect_state\":\"waiting-for-server\"");
        var cli = new FakeCli().Answer("settings --json", on).Fail("gpus --json", "dsa: no GPU list");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.Equal(AdbAutoConnectState.WaitingForServer, vm.AdbAutoConnectState);

        cli.Answer("settings --json", on.Replace("waiting-for-server", "attached"));
        var changed = new List<string?>();
        vm.PropertyChanged += (_, e) => changed.Add(e.PropertyName);
        await vm.PollAdbAutoConnectAsync();
        Assert.Equal(AdbAutoConnectState.Attached, vm.AdbAutoConnectState);
        Assert.Equal("AdbAutoConnect_Attached", vm.AdbAutoConnectStateText);
        Assert.DoesNotContain(string.Empty, changed);
        Assert.Contains(nameof(SettingsViewModel.AdbAutoConnectStateText), changed);
    }

    [Fact]
    public async Task Fr20_nothing_is_read_again_while_developer_mode_is_off()
    {
        var cli = new FakeCli().Answer("settings --json", Off).Fail("gpus --json", "dsa: no GPU list");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        var before = cli.Calls.Count(call => call == "settings --json");
        await vm.PollAdbAutoConnectAsync();
        Assert.Equal(before, cli.Calls.Count(call => call == "settings --json"));
    }

    private static XDocument Strings(string file)
    {
        // The tests run from dsa/target/settings/..., outside the source tree, so each folder
        // above is tried both as the settings folder and as the repository's dsa folder.
        string[] relative = { Path.Combine("DsaSettings", file),
            Path.Combine("host", "platform", "windows", "settings", "DsaSettings", file) };
        string? found = null;
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null && found is null; dir = dir.Parent)
        {
            found = relative.Select(r => Path.Combine(dir.FullName, r)).FirstOrDefault(File.Exists);
        }
        Assert.True(found is not null, file + " was not found above " + AppContext.BaseDirectory);
        return XDocument.Load(found!);
    }

    [Theory]
    [InlineData("Strings.resx")]
    [InlineData("Strings.ko.resx")]
    public void Fr20_every_auto_connect_text_is_in_both_languages(string file)
    {
        var names = Strings(file).Descendants("data")
            .Select(d => (string?)d.Attribute("name"))
            .ToHashSet();
        Assert.Contains("AdbAutoConnectCard.Header", names);
        Assert.Contains("AdbAutoConnectCard.Description", names);
        foreach (var state in Enum.GetValues<AdbAutoConnectState>())
        {
            Assert.Contains($"AdbAutoConnect_{state}", names);
        }
    }
}
