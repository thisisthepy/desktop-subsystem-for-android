using System.Windows;
using System.Windows.Controls;
using Dsa.Settings.Core;

namespace Dsa.Settings.Pages;

public partial class UpdatesPage : Page
{
    private static SettingsViewModel ViewModel => App.Settings;

    public UpdatesPage()
    {
        InitializeComponent();
        DataContext = ViewModel;
        Loaded += async (_, _) =>
        {
            await ViewModel.LoadAsync();
            await App.General.PollAsync();
            NoticeBar.Message = App.General.UpdateNotice;
            NoticeBar.IsOpen = App.General.HasUpdateNotice;
        };
    }

    // Only a click sends a change; the switch shows what the service keeps once it answers.
    private async void OnProgramToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetProgramUpdatesAsync(ProgramSwitch.IsChecked == true);

    private async void OnImageToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetImageUpdatesAsync(ImageSwitch.IsChecked == true);

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

    private async void OnUpdateNow(object sender, RoutedEventArgs e)
    {
        var confirmed = await Shell.Confirm(App.Text.Get("UpdateNowTitle"),
            App.Text.Get("UpdateNowBody"), App.Text.Get("UpdateNowAction"));
        if (!confirmed)
        {
            return;
        }
        UpdateNowButton.IsEnabled = false;
        UpdateRing.Visibility = Visibility.Visible;
        try
        {
            var said = await App.Service.UpdateNowAsync();
            await Shell.Tell(App.Text.Get("UpdateNowTitle"), said);
        }
        catch (Exception error)
        {
            await Shell.Tell(App.Text.Get("UpdateNowFailed"), Format.Problem(App.Text, error));
        }
        finally
        {
            UpdateRing.Visibility = Visibility.Collapsed;
            UpdateNowButton.IsEnabled = ViewModel.CanUpdateNow;
        }
    }
}
