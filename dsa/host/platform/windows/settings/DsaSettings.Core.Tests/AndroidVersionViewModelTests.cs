using Xunit;

namespace Dsa.Settings.Core.Tests;

public class AndroidVersionViewModelTests
{
    private const string Running17 =
        "{\"version\":\"17.0.0-16373615-8\",\"android_major\":17,\"build\":1637361508," +
        "\"build_id\":\"16373615\",\"security_patch\":\"2026-06-05\",\"channel\":\"latest\"," +
        "\"supported_majors\":[16,17],\"target_major\":17,\"on_trial\":false,\"notice\":null}";

    private static FakeCli Cli() => new FakeCli().Answer("android version --json", Running17);

    /// <summary>An installation that bundled no image, before any image has started (#1065).</summary>
    private const string NoImage =
        "{\"version\":\"none\",\"android_major\":0,\"build\":0,\"build_id\":null,\"security_patch\":null," +
        "\"channel\":\"latest\",\"supported_majors\":[16,17],\"target_major\":17,\"on_trial\":false," +
        "\"notice\":\"No Android image is installed, and no image to install is available yet (supported: Android 16, 17); " +
        "the service keeps checking\"}";

    [Fact]
    public async Task Fr14_with_no_image_the_page_says_none_is_installed_without_android_0_or_unknown_rows()
    {
        var vm = new AndroidVersionViewModel(new FakeCli().Answer("android version --json", NoImage), new KeyText());
        await vm.LoadAsync();
        Assert.False(vm.HasError, vm.Error);
        Assert.False(vm.HasImage);
        Assert.Equal("Android_NoImage", vm.VersionText);
        Assert.Equal("", vm.ImageVersion);
        // The service's news is shown in the app's language.
        Assert.Equal("UpdateNotice_AndroidNoImageAvailable", vm.UpdateStatusText);

        var running = new AndroidVersionViewModel(Cli(), new KeyText());
        await running.LoadAsync();
        Assert.True(running.HasImage);
        Assert.Equal("17.0.0-16373615-8", running.ImageVersion);
    }

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

    [Fact]
    public async Task Fr14_the_updates_page_shows_the_version_followed_and_the_download_with_its_progress()
    {
        var downloading = Running17.Replace("\"notice\":null",
            "\"notice\":\"downloading Android 17.0.0-16373615-9: 45% (0.81 of 1.8 GB)\"");
        var cli = new FakeCli().Answer("android version --json", downloading);
        var vm = new AndroidVersionViewModel(cli, new KeyText());
        await vm.LoadAsync();
        Assert.Equal("Android_FollowsLatest", vm.ChannelText);
        Assert.True(vm.IsDownloading);
        Assert.Equal(45, vm.DownloadPercent);
        Assert.StartsWith("downloading Android", vm.UpdateStatusText);

        var idle = new AndroidVersionViewModel(Cli(), new KeyText());
        await idle.LoadAsync();
        Assert.False(idle.IsDownloading);
        Assert.Null(idle.DownloadPercent);
        Assert.Equal("Android_UpToDate", idle.UpdateStatusText);
        Assert.Null(AndroidVersionViewModel.DownloadPercentOf("Android 17.0.0-1 is installed"));
    }

    [Fact]
    public async Task Fr14_a_downloaded_image_is_applied_without_the_user_restarting()
    {
        var waiting = Running17.Replace("\"notice\":null",
            "\"notice\":null,\"restart_pending\":\"17.0.0-16373615-9\"");
        var cli = new FakeCli()
            .Answer("android version --json", waiting)
            .Answer("android restart --json", "{\"restarting\":\"17.0.0-16373615-9\",\"detail\":\"restarting\"}");
        var vm = new AndroidVersionViewModel(cli, new KeyText());
        await vm.LoadAsync();
        Assert.True(vm.IsRestartPending);
        Assert.True(vm.CanRestartNow);
        Assert.Equal("17.0.0-16373615-9", vm.RestartPending);
        Assert.Equal("Android_RestartPending", vm.UpdateStatusText);

        // [Restart now] asks the service, then reads the version again.
        cli.Answer("android version --json", Running17);
        await vm.RestartNowAsync();
        Assert.False(vm.HasError, vm.Error);
        Assert.Contains("android restart --json", cli.Calls);
        Assert.False(vm.IsRestartPending);
        Assert.Equal("Android_UpToDate", vm.UpdateStatusText);

        // Nothing waits: no button.
        var idle = new AndroidVersionViewModel(Cli(), new KeyText());
        await idle.LoadAsync();
        Assert.False(idle.IsRestartPending);
        Assert.False(idle.CanRestartNow);
    }

    [Fact]
    public async Task Fr14_image_auto_restart_is_on_by_default_and_turned_off_through_the_service()
    {
        const string Settings = "{\"render_gpu\":\"display\",\"version\":\"0.0.9\"}";
        var cli = new FakeCli()
            .Answer("settings --json", Settings)
            .Fail("gpus --json", "dsa: no GPU list")
            .Answer("settings set image-auto-restart off --json",
                "{\"render_gpu\":\"display\",\"version\":\"0.0.9\",\"image_auto_restart\":false}");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.True(vm.ImageAutoRestart, "on when the service does not say");
        await vm.SetImageAutoRestartAsync(false);
        Assert.Contains("settings set image-auto-restart off --json", cli.Calls);
        Assert.False(vm.ImageAutoRestart);
    }

    [Fact]
    public async Task Fr14_check_now_asks_for_the_version_already_followed()
    {
        var pinned = Running17.Replace("\"channel\":\"latest\"", "\"channel\":\"17\"");
        var cli = new FakeCli().Answer("android version --json", pinned).Answer("android use 17 --json", pinned);
        var vm = new AndroidVersionViewModel(cli, new KeyText());
        await vm.LoadAsync();
        Assert.Equal("Android_FollowsMajor", vm.ChannelText);
        Assert.True(vm.CanCheckNow);
        Assert.Equal(new[] { "android", "use", "17", "--json" }, vm.CheckArguments);
        await vm.CheckNowAsync();
        Assert.False(vm.HasError, vm.Error);
        Assert.Contains("android use 17 --json", cli.Calls);
    }
}
