using Xunit;

namespace Dsa.Settings.Core.Tests;

public class BackgroundViewModelTests
{
    private const string Music =
        "{\"package\":\"com.example.music\",\"label\":\"Music\",\"title\":\"Playing\",\"text\":\"Song - Artist\"," +
        "\"actions\":[\"Pause\",\"Next\"],\"icon_png\":\"iVBORw==\"}";

    private const string Vpn =
        "{\"package\":\"com.example.vpn\",\"label\":\"\",\"title\":\"\",\"text\":\"\",\"actions\":[],\"icon_png\":\"\"}";

    private const string Song =
        "{\"package\":\"com.example.music\",\"label\":\"Music\",\"title\":\"Song\",\"artist\":\"Artist\"," +
        "\"album\":\"Album\",\"playback\":\"playing\"}";

    private const string Asking = "{\"render_gpu\":\"display\",\"stop_without_asking\":false}";

    [Fact]
    public async Task Fr24_the_page_lists_the_apps_that_run_in_the_background()
    {
        var cli = new FakeCli()
            .Answer("settings --json", Asking)
            .Answer("background list --json", "{\"apps\":[" + Music + "," + Vpn + "],\"media\":[" + Song + "]}");
        var vm = new BackgroundViewModel(new DsaService(cli), new KeyText());
        await vm.RefreshAsync();
        Assert.False(vm.HasError);
        Assert.False(vm.HasNone);
        Assert.True(vm.AskBeforeStop);
        Assert.Equal(2, vm.Apps.Count);
        Assert.Equal("Music", vm.Apps[0].Label);
        Assert.Equal("Playing · Song - Artist\nplaying Song - Artist", vm.Apps[0].Detail);
        Assert.Equal(new byte[] { 0x89, (byte)'P', (byte)'N', (byte)'G' }, vm.Apps[0].Icon);
        // An app with no name or notification text shows its package, and no icon.
        Assert.Equal("com.example.vpn", vm.Apps[1].Label);
        Assert.Equal("com.example.vpn", vm.Apps[1].Detail);
        Assert.Null(vm.Apps[1].Icon);
    }

    [Fact]
    public async Task Nfr1_the_background_page_lets_go_of_its_list_when_left()
    {
        var cli = new FakeCli()
            .Answer("settings --json", Asking)
            .Answer("background list --json", "{\"apps\":[" + Music + "],\"media\":[]}");
        var vm = new BackgroundViewModel(new DsaService(cli), new KeyText());
        await vm.RefreshAsync();
        Assert.Single(vm.Apps);
        vm.Release();
        Assert.Empty(vm.Apps);
        Assert.False(vm.Loaded);
        // Shown again, it reads the list afresh.
        await vm.RefreshAsync();
        Assert.Single(vm.Apps);
    }

    [Fact]
    public async Task Fr24_stop_ends_the_app_and_the_list_follows()
    {
        var cli = new FakeCli()
            .Answer("settings --json", Asking)
            .Answer("background list --json", "{\"apps\":[" + Music + "],\"media\":[]}")
            .Answer("background stop com.example.music --json", "{\"stopped\":\"com.example.music\"}");
        var vm = new BackgroundViewModel(new DsaService(cli), new KeyText());
        await vm.RefreshAsync();
        cli.Answer("background list --json", "{\"apps\":[],\"media\":[]}");
        await vm.StopAsync("com.example.music");
        Assert.False(vm.HasError);
        Assert.Equal("stopped Music", vm.Message);
        Assert.True(vm.HasNone);
        Assert.Contains("background stop com.example.music --json", cli.Calls);
    }

    [Fact]
    public async Task Fr24_asking_before_a_stop_can_be_turned_back_on()
    {
        var cli = new FakeCli()
            .Answer("settings --json", "{\"stop_without_asking\":true}")
            .Answer("background list --json", "{\"apps\":[],\"media\":[]}")
            .Answer("settings set stop-without-asking off --json", "{\"stop_without_asking\":false}");
        var vm = new BackgroundViewModel(new DsaService(cli), new KeyText());
        await vm.RefreshAsync();
        Assert.False(vm.AskBeforeStop);
        await vm.SetAskBeforeStopAsync(true);
        Assert.True(vm.AskBeforeStop);
        Assert.Contains("settings set stop-without-asking off --json", cli.Calls);
    }

    [Fact]
    public async Task Fr24_a_stop_android_refused_says_why()
    {
        var cli = new FakeCli()
            .Answer("background list --json", "{\"apps\":[],\"media\":[]}")
            .Fail("background stop com.example.music --json", "dsa: Android is Stopped, not running");
        var vm = new BackgroundViewModel(new DsaService(cli), new KeyText());
        await vm.StopAsync("com.example.music");
        Assert.True(vm.HasError);
        Assert.Contains("not running", vm.Error);
        Assert.False(vm.HasMessage);
    }
}
