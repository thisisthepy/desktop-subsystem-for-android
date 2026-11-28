using System.Globalization;
using Xunit;

namespace Dsa.Settings.Core.Tests;

public class GamesViewModelTests
{
    private static string App(string package, string label, bool game, bool keepFocus, string icon = "") =>
        "{\"package\":\"" + package + "\",\"label\":\"" + label + "\",\"game\":" + (game ? "true" : "false") +
        ",\"keep_focus\":" + (keepFocus ? "true" : "false") + ",\"icon_png\":\"" + icon + "\"}";

    private static string List(params string[] apps) => "{\"apps\":[" + string.Join(",", apps) + "]}";

    private static string Valley(bool on) => App("com.ustwo.monumentvalley", "Monument Valley", true, on, "iVBORw==");

    private static string Notes(bool on) => App("com.example.notes", "", false, on);

    private static GamesViewModel ViewModel(FakeCli cli) => new(new DsaService(cli), new KeyText());

    public GamesViewModelTests()
    {
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
    }

    [Fact]
    public async Task Fr44_the_page_lists_every_app_games_first_with_keep_focus_off_by_default()
    {
        var cli = new FakeCli().Answer("games list --json", List(Valley(false), Notes(false)));
        var vm = ViewModel(cli);
        await vm.RefreshAsync();
        Assert.False(vm.HasError);
        Assert.False(vm.HasNone);
        Assert.True(vm.HasApps);
        Assert.Equal(2, vm.Apps.Count);
        // The service's order, games first.
        Assert.Equal("Monument Valley", vm.Apps[0].Label);
        Assert.True(vm.Apps[0].Game);
        Assert.Equal("Games_GameDetail", vm.Apps[0].Detail);
        Assert.Equal(new byte[] { 0x89, (byte)'P', (byte)'N', (byte)'G' }, vm.Apps[0].Icon);
        // An app with no name shows its package, and no icon.
        Assert.Equal("com.example.notes", vm.Apps[1].Label);
        Assert.Equal("com.example.notes", vm.Apps[1].Detail);
        Assert.Null(vm.Apps[1].Icon);
        Assert.All(vm.Apps, a => Assert.False(a.KeepFocus));
        Assert.False(vm.AllOn);
    }

    [Fact]
    public async Task Fr44_a_switch_turns_keep_focus_on_and_off_for_its_app()
    {
        var cli = new FakeCli()
            .Answer("games list --json", List(Valley(false), Notes(false)))
            .Answer("games keep-focus on com.ustwo.monumentvalley --json", List(Valley(true), Notes(false)))
            .Answer("games keep-focus off com.ustwo.monumentvalley --json", List(Valley(false), Notes(false)));
        var vm = ViewModel(cli);
        await vm.RefreshAsync();
        await vm.SetKeepFocusAsync("com.ustwo.monumentvalley", true);
        Assert.True(vm.Apps[0].KeepFocus);
        Assert.False(vm.Apps[1].KeepFocus);
        await vm.SetKeepFocusAsync("com.ustwo.monumentvalley", false);
        Assert.False(vm.Apps[0].KeepFocus);
        Assert.Contains("games keep-focus off com.ustwo.monumentvalley --json", cli.Calls);
    }

    [Fact]
    public async Task Fr44_select_all_turns_every_app_on_and_pressed_again_off()
    {
        var cli = new FakeCli()
            .Answer("games list --json", List(Valley(true), Notes(false)))
            .Answer("games select-all --json", List(Valley(true), Notes(true)));
        var vm = ViewModel(cli);
        await vm.RefreshAsync();
        Assert.False(vm.AllOn);
        await vm.SelectAllAsync();
        Assert.All(vm.Apps, a => Assert.True(a.KeepFocus));
        Assert.True(vm.AllOn);
        // Pressed again with every app on, dsa turns every app off.
        cli.Answer("games select-all --json", List(Valley(false), Notes(false)));
        await vm.SelectAllAsync();
        Assert.All(vm.Apps, a => Assert.False(a.KeepFocus));
        Assert.False(vm.AllOn);
        Assert.Equal(2, cli.Calls.Count(c => c == "games select-all --json"));
    }

    [Fact]
    public async Task Fr44_the_page_says_when_no_app_is_installed_and_when_dsa_fails()
    {
        var cli = new FakeCli().Answer("games list --json", List());
        var vm = ViewModel(cli);
        await vm.RefreshAsync();
        Assert.True(vm.HasNone);
        Assert.False(vm.HasApps);
        Assert.False(vm.AllOn);

        var failing = ViewModel(new FakeCli().Fail("games list --json", "dsa: cannot reach the DSA service"));
        await failing.RefreshAsync();
        Assert.True(failing.HasError);
        Assert.Empty(failing.Apps);
    }

    [Fact]
    public async Task Nfr1_the_games_page_lets_go_of_its_list_when_left()
    {
        var cli = new FakeCli().Answer("games list --json", List(Valley(false)));
        var vm = ViewModel(cli);
        await vm.RefreshAsync();
        Assert.Single(vm.Apps);
        vm.Release();
        Assert.Empty(vm.Apps);
        Assert.False(vm.Loaded);
        await vm.RefreshAsync();
        Assert.Single(vm.Apps);
    }
}
