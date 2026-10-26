using Dsa.Settings.Core;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Dsa.Settings.Pages;

public sealed partial class UpdatesPage : Page
{
    internal SettingsViewModel ViewModel => App.Settings;
    internal GeneralViewModel General => App.General;

    public UpdatesPage()
    {
        InitializeComponent();
        Loaded += async (_, _) =>
        {
            await ViewModel.LoadAsync();
            await General.PollAsync();
        };
    }

    // A switch also moves when the service's answer is shown; only a change the user made,
    // which differs from what the service keeps, is sent.
    private async void OnProgramToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetProgramUpdatesAsync(ProgramSwitch.IsOn);

    private async void OnImageToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetImageUpdatesAsync(ImageSwitch.IsOn);

    private async void OnUpdateNow(object sender, RoutedEventArgs e)
    {
        var confirmed = await Shell.Confirm(XamlRoot, App.Text.Get("UpdateNowTitle"),
            App.Text.Get("UpdateNowBody"), App.Text.Get("UpdateNowAction"));
        if (!confirmed)
        {
            return;
        }
        UpdateNowButton.IsEnabled = false;
        UpdateRing.IsActive = true;
        try
        {
            var said = await App.Service.UpdateNowAsync();
            await Shell.Tell(XamlRoot, App.Text.Get("UpdateNowTitle"), said);
        }
        catch (Exception error)
        {
            await Shell.Tell(XamlRoot, App.Text.Get("UpdateNowFailed"), Format.Problem(App.Text, error));
        }
        finally
        {
            UpdateRing.IsActive = false;
            UpdateNowButton.IsEnabled = ViewModel.CanChangeProgramUpdates;
        }
    }
}
