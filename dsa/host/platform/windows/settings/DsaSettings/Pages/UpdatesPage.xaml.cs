using System.Windows;
using System.Windows.Controls;
using System.Windows.Threading;
using Dsa.Settings.Core;

namespace Dsa.Settings.Pages;

/// <summary>Updates: the Android image (the version that runs, the version it follows, its
/// update with the download's progress, and choosing a supported version) and DSA itself
/// (automatic updates, the release channel, installing now).</summary>
public partial class UpdatesPage : Page
{
    /// <summary>Follows an image download while the page is shown.</summary>
    private readonly DispatcherTimer _androidTimer = new() { Interval = TimeSpan.FromSeconds(3) };

    /// <summary>Follows an update of DSA while the page is shown, across the service's
    /// restart.</summary>
    private readonly DispatcherTimer _programTimer = new() { Interval = TimeSpan.FromSeconds(1.5) };

    private static SettingsViewModel ViewModel => App.Settings;

    private static AndroidVersionViewModel Android => App.AndroidVersion;

    private static ProgramUpdateViewModel ProgramUpdate => App.ProgramUpdate;

    public UpdatesPage()
    {
        InitializeComponent();
        DataContext = ViewModel;
        AndroidStatus.DataContext = Android;
        AndroidChoice.DataContext = Android;
        ProgramProgress.DataContext = ProgramUpdate;
        // The gate follows the update; the button inside it follows the settings.
        UpdateNowGate.DataContext = ProgramUpdate;
        UpdateNowButton.DataContext = ViewModel;
        _androidTimer.Tick += PollingTimers.Once(Android.RefreshAsync);
        _programTimer.Tick += PollingTimers.Once(ProgramUpdate.PollAsync);
        PollingTimers.Attach(this, _androidTimer, _programTimer);
        Loaded += async (_, _) =>
        {
            ProgramUpdate.CameBack += OnServiceBack;
            await ProgramUpdate.PollAsync();
            await Task.WhenAll(ViewModel.LoadAsync(), Android.LoadAsync());
            await ShowNoticeAsync();
        };
        Unloaded += (_, _) => ProgramUpdate.CameBack -= OnServiceBack;
    }

    private async Task ShowNoticeAsync()
    {
        await App.General.PollAsync();
        NoticeBar.Message = App.General.UpdateNotice;
        NoticeBar.IsOpen = App.General.HasUpdateNotice;
    }

    /// <summary>The new service answers after an update: the page reads what it says now, its
    /// version among it.</summary>
    private async void OnServiceBack(object? sender, EventArgs e)
    {
        await Task.WhenAll(ViewModel.LoadAsync(), Android.LoadAsync());
        await ShowNoticeAsync();
    }

    // Only a click sends a change; the switch shows what the service keeps once it answers.
    private async void OnProgramToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetProgramUpdatesAsync(ProgramSwitch.IsChecked == true);

    private async void OnImageToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetImageUpdatesAsync(ImageSwitch.IsChecked == true);

    private async void OnAutoRestartToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetImageAutoRestartAsync(AutoRestartSwitch.IsChecked == true);

    /// <summary>Restarts Android now into the downloaded image, after the user agrees that its
    /// app windows close.</summary>
    private async void OnRestartNow(object sender, RoutedEventArgs e)
    {
        if (Android.RestartPending is not { } version)
        {
            return;
        }
        var confirmed = await Shell.Confirm(App.Text.Get("AndroidRestartConfirm.Title"),
            string.Format(System.Globalization.CultureInfo.CurrentCulture,
                App.Text.Get("AndroidRestartConfirm.Body"), version),
            App.Text.Get("AndroidRestartButton.Content"));
        if (confirmed)
        {
            await Android.RestartNowAsync();
        }
    }

    private async void OnPrereleaseToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetPrereleaseUpdatesAsync(PrereleaseSwitch.IsChecked == true);

    private void OnPrereleaseAsAdministrator(object sender, RoutedEventArgs e) =>
        ChangeAsAdministrator("prerelease-updates", !ViewModel.PrereleaseUpdates);

    private void OnProgramAsAdministrator(object sender, RoutedEventArgs e) =>
        ChangeAsAdministrator("program-updates", !ViewModel.ProgramUpdates);

    private void OnImageAsAdministrator(object sender, RoutedEventArgs e) =>
        ChangeAsAdministrator("image-updates", !ViewModel.ImageUpdates);

    /// <summary>Runs <c>dsa settings set</c> as an administrator (Windows asks the user),
    /// then shows what the service keeps.</summary>
    private static async void ChangeAsAdministrator(string name, bool on)
    {
        var problem = await Shell.RunDsaElevated("settings", "set", name, on ? "on" : "off");
        if (problem is not null)
        {
            await Shell.Tell(App.Text.Get("SettingsErrorBar.Title"), problem);
        }
        await ViewModel.LoadAsync();
    }

    /// <summary>Asks the service to look for an Android image now. The image is shared by
    /// every user of this computer, so an administrator asks; Windows asks the user to allow
    /// it.</summary>
    private async void OnCheckNow(object sender, RoutedEventArgs e)
    {
        if (Shell.IsAdministrator())
        {
            await Android.CheckNowAsync();
            return;
        }
        var problem = await Shell.RunDsaElevated(Android.CheckArguments);
        if (problem is not null)
        {
            await Shell.Tell(App.Text.Get("SettingsErrorBar.Title"), problem);
        }
        await Android.LoadAsync();
    }

    private async void OnApply(object sender, RoutedEventArgs e)
    {
        if (Android.Selected is not { } choice)
        {
            return;
        }
        var move = Android.MoveTo(choice);
        var confirmed = move switch
        {
            // Moving down loses data, so the user reads what goes first.
            AndroidMove.Down => await Shell.Confirm(App.Text.Get("AndroidDownConfirm.Title"),
                Android.DownWarning(choice), App.Text.Get("AndroidDownConfirm.Action")),
            AndroidMove.Up => await Shell.Confirm(App.Text.Get("AndroidUpConfirm.Title"),
                string.Format(App.Text.Get("AndroidUpConfirm.Body"), choice.Major), App.Text.Get("AndroidApplyButton.Content")),
            _ => true,
        };
        if (!confirmed)
        {
            await Android.LoadAsync();
            return;
        }
        var erase = move == AndroidMove.Down;
        // The image is shared by every user of this computer, so the service takes the choice
        // from an administrator only; Windows asks the user to allow it.
        if (Shell.IsAdministrator())
        {
            await Android.UseAsync(choice, erase);
            return;
        }
        var args = AndroidVersionViewModel.UseArguments(choice, erase);
        var problem = await Shell.RunDsaElevated(args);
        if (problem is not null)
        {
            await Shell.Tell(App.Text.Get("SettingsErrorBar.Title"), problem);
        }
        await Android.LoadAsync();
    }

    private async void OnUpdateNow(object sender, RoutedEventArgs e)
    {
        var confirmed = await Shell.Confirm(App.Text.Get("UpdateNowTitle"),
            App.Text.Get("UpdateNowBody"), App.Text.Get("UpdateNowAction"));
        if (!confirmed)
        {
            return;
        }
        // The page shows the update from here on: its phase, the version, and that DSA should
        // be left alone, until the new service answers. No dialog waits on it meanwhile. The
        // page's timer already runs while the window is shown, so nothing starts it here: a
        // minimised window does not poll.
        var answer = await ProgramUpdate.UpdateNowAsync();
        if (ProgramUpdate.HasError)
        {
            await Shell.Tell(App.Text.Get("UpdateNowFailed"), ProgramUpdate.Error);
        }
        else if (answer is { Installing: null })
        {
            // Nothing newer: the service says why.
            await Shell.Tell(App.Text.Get("UpdateNowTitle"), answer.Detail);
        }
    }
}
