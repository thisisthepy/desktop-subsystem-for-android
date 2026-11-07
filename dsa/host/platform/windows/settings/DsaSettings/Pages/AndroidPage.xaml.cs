using System.Windows;
using System.Windows.Controls;
using Dsa.Settings.Core;

namespace Dsa.Settings.Pages;

/// <summary>The Android version that runs, and choosing one of the supported ones.</summary>
public partial class AndroidPage : Page
{
    private static AndroidVersionViewModel ViewModel => App.AndroidVersion;

    public AndroidPage()
    {
        InitializeComponent();
        DataContext = ViewModel;
        Loaded += async (_, _) => await ViewModel.LoadAsync();
    }

    private async void OnApply(object sender, RoutedEventArgs e)
    {
        if (ViewModel.Selected is not { } choice)
        {
            return;
        }
        var move = ViewModel.MoveTo(choice);
        var confirmed = move switch
        {
            // Moving down loses data, so the user reads what goes first.
            AndroidMove.Down => await Shell.Confirm(App.Text.Get("AndroidDownConfirm.Title"),
                ViewModel.DownWarning(choice), App.Text.Get("AndroidDownConfirm.Action")),
            AndroidMove.Up => await Shell.Confirm(App.Text.Get("AndroidUpConfirm.Title"),
                string.Format(App.Text.Get("AndroidUpConfirm.Body"), choice.Major), App.Text.Get("AndroidApplyButton.Content")),
            _ => true,
        };
        if (!confirmed)
        {
            await ViewModel.LoadAsync();
            return;
        }
        var erase = move == AndroidMove.Down;
        // The image is shared by every user of this computer, so the service takes the choice
        // from an administrator only; Windows asks the user to allow it.
        if (Shell.IsAdministrator())
        {
            await ViewModel.UseAsync(choice, erase);
            return;
        }
        var args = AndroidVersionViewModel.UseArguments(choice, erase);
        var problem = await Shell.RunDsaElevated(args);
        if (problem is not null)
        {
            await Shell.Tell(App.Text.Get("SettingsErrorBar.Title"), problem);
        }
        await ViewModel.LoadAsync();
    }
}
