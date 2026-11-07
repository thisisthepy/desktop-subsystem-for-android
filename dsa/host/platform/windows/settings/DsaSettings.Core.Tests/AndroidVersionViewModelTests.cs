using Xunit;

namespace Dsa.Settings.Core.Tests;

public class AndroidVersionViewModelTests
{
    private const string Running17 =
        "{\"version\":\"17.0.0-16373615-8\",\"android_major\":17,\"build\":1637361508," +
        "\"build_id\":\"16373615\",\"security_patch\":\"2026-06-05\",\"channel\":\"latest\"," +
        "\"supported_majors\":[16,17],\"target_major\":17,\"on_trial\":false,\"notice\":null}";

    private static FakeCli Cli() => new FakeCli().Answer("android version --json", Running17);

    [Fact]
    public async Task Fr14_the_page_shows_the_running_version_build_and_security_patch()
    {
        var vm = new AndroidVersionViewModel(Cli(), new KeyText());
        await vm.LoadAsync();
        Assert.False(vm.HasError, vm.Error);
        Assert.Equal("17.0.0-16373615-8", vm.ImageVersion);
        Assert.Equal("16373615", vm.BuildId);
        Assert.Equal("2026-06-05", vm.SecurityPatch);
        Assert.False(vm.IsSwitching);
        // The newest, then each supported major, newest first.
        Assert.Equal(new[] { "latest", "17", "16" }, vm.Choices.Select(c => c.Channel));
        Assert.Equal(0, vm.SelectedIndex);
        Assert.False(vm.CanApply, "nothing was picked");
    }

    [Fact]
    public async Task Fr14_an_older_major_is_a_move_down_and_is_sent_with_erase_data()
    {
        var moved = Running17.Replace("\"channel\":\"latest\"", "\"channel\":\"16\"")
            .Replace("\"target_major\":17", "\"target_major\":16");
        var cli = Cli().Answer("android use 16 --erase-data --json", moved);
        var vm = new AndroidVersionViewModel(cli, new KeyText());
        await vm.LoadAsync();
        vm.SelectedIndex = 2;
        Assert.True(vm.CanApply);
        Assert.Equal(AndroidMove.Down, vm.SelectedMove);
        Assert.Equal(AndroidMove.Same, vm.MoveTo(vm.Choices[1]));
        Assert.Equal(new[] { "android", "use", "16", "--json" },
            AndroidVersionViewModel.UseArguments(vm.Choices[2], false));

        await vm.UseAsync(vm.Choices[2], true);
        Assert.False(vm.HasError, vm.Error);
        Assert.Contains("android use 16 --erase-data --json", cli.Calls);
        Assert.True(vm.IsSwitching);
        Assert.Equal("16", vm.Selected!.Channel);
    }

    [Fact]
    public async Task Fr14_a_refused_choice_shows_why()
    {
        var cli = Cli().Fail("android use 16 --json", "dsa: only an administrator can choose the Android version");
        var vm = new AndroidVersionViewModel(cli, new KeyText());
        await vm.LoadAsync();
        await vm.UseAsync(vm.Choices[2], false);
        Assert.True(vm.HasError);
        Assert.Contains("only an administrator", vm.Error);
    }
}
