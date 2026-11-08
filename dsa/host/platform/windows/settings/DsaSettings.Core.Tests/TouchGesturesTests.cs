using System.Globalization;
using System.Xml.Linq;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>The Display page's switch that turns Windows' touch gestures off while a DSA app
/// window is in front: on by default, disabled with the reason when Windows refused, and hidden
/// on a Windows without such gestures.</summary>
public class TouchGesturesTests
{
    private const string Available =
        "{\"render_gpu\":\"display\",\"freeze\":false,\"program_updates\":true,\"image_updates\":true," +
        "\"program_updates_available\":true,\"image_updates_available\":false,\"version\":\"0.0.9\"," +
        "\"touch_gestures_off\":true,\"touch_gestures_state\":\"available\",\"touch_gestures_problem\":\"\"}";

    public TouchGesturesTests()
    {
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
    }

    private static FakeCli Cli(string settings) => new FakeCli()
        .Answer("settings --json", settings)
        .Fail("gpus --json", "dsa: no GPU list");

    [Fact]
    public async Task Fr5_touch_gestures_off_is_on_by_default_and_the_switch_turns_it_off()
    {
        var cli = Cli(Available)
            .Answer("settings set touch-gestures-off off --json", Available.Replace("\"touch_gestures_off\":true", "\"touch_gestures_off\":false"))
            .Answer("settings set touch-gestures-off on --json", Available);
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.True(vm.TouchGesturesOff);
        Assert.True(vm.ShowTouchGestures);
        Assert.True(vm.CanChangeTouchGestures);
        Assert.False(vm.TouchGesturesFailed);
        Assert.Equal("", vm.TouchGesturesProblemText);

        await vm.SetTouchGesturesOffAsync(false);
        Assert.False(vm.TouchGesturesOff);
        await vm.SetTouchGesturesOffAsync(true);
        Assert.True(vm.TouchGesturesOff);
        Assert.Contains("settings set touch-gestures-off off --json", cli.Calls);
        Assert.Contains("settings set touch-gestures-off on --json", cli.Calls);
    }

    [Fact]
    public async Task Fr5_a_refused_call_disables_the_switch_and_says_why()
    {
        var failed = Available.Replace("\"touch_gestures_state\":\"available\"", "\"touch_gestures_state\":\"failed\"")
            .Replace("\"touch_gestures_problem\":\"\"", "\"touch_gestures_problem\":\"The parameter is incorrect.\"");
        var cli = Cli(failed);
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.True(vm.ShowTouchGestures);
        Assert.True(vm.TouchGesturesFailed);
        Assert.False(vm.CanChangeTouchGestures);
        Assert.Equal("cannot be used: The parameter is incorrect.", vm.TouchGesturesProblemText);
        Assert.Equal("ms-settings:devices-touch", SettingsViewModel.TouchSettingsUri);

        await vm.SetTouchGesturesOffAsync(false);
        Assert.DoesNotContain(cli.Calls, call => call.Contains("touch-gestures-off"));
    }

    [Theory]
    [InlineData("unsupported", 26100, false)]
    [InlineData("available", 26100, true)]
    [InlineData("failed", 26100, true)]
    [InlineData("", 26100, true)]
    [InlineData("", 19045, false)]
    [InlineData("", 20348, false)]
    public async Task Fr5_the_switch_is_hidden_on_a_windows_without_touch_gestures(string state, int build, bool shown)
    {
        var settings = Available.Replace("\"touch_gestures_state\":\"available\"", $"\"touch_gestures_state\":\"{state}\"");
        var vm = new SettingsViewModel(new DsaService(Cli(settings)), new KeyText()) { WindowsBuild = build };
        await vm.LoadAsync();
        Assert.Equal(shown, vm.ShowTouchGestures);
    }

    [Fact]
    public async Task Fr5_a_service_that_predates_the_setting_reads_as_on_and_unknown()
    {
        var old = Available.Replace(
            ",\"touch_gestures_off\":true,\"touch_gestures_state\":\"available\",\"touch_gestures_problem\":\"\"", "");
        var vm = new SettingsViewModel(new DsaService(Cli(old)), new KeyText()) { WindowsBuild = 26100 };
        await vm.LoadAsync();
        Assert.True(vm.TouchGesturesOff);
        Assert.Equal(TouchGesturesState.Unknown, vm.TouchGesturesState);
        Assert.True(vm.CanChangeTouchGestures);
    }

    [Theory]
    [InlineData("unsupported", TouchGesturesState.Unsupported)]
    [InlineData("available", TouchGesturesState.Available)]
    [InlineData("failed", TouchGesturesState.Failed)]
    [InlineData("something-new", TouchGesturesState.Unknown)]
    [InlineData("", TouchGesturesState.Unknown)]
    [InlineData(null, TouchGesturesState.Unknown)]
    public void Fr5_each_state_name_is_read(string? name, TouchGesturesState state) =>
        Assert.Equal(state, DsaService.ReadTouchGesturesState(name));

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
    public void Fr5_every_touch_gestures_text_is_in_both_languages(string file)
    {
        var values = Strings(file).Descendants("data")
            .ToDictionary(d => (string?)d.Attribute("name") ?? "", d => d.Element("value")?.Value ?? "");
        foreach (var name in new[] { "TouchGesturesCard.Header", "TouchGesturesCard.Description",
            "TouchGestures_Failed", "TouchGestures_NoReason", "TouchSettingsLink.Content" })
        {
            Assert.True(values.ContainsKey(name), $"{file} has no {name}");
        }
        Assert.Contains("{0}", values["TouchGestures_Failed"]);
    }
}
