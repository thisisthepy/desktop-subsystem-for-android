using System.Windows;
using System.Windows.Controls;
using Dsa.Settings.Core;

namespace Dsa.Settings.Pages;

public partial class DisplayPage : Page
{
    private static SettingsViewModel ViewModel => App.Settings;

    public DisplayPage()
    {
        InitializeComponent();
        DataContext = ViewModel;
        Loaded += async (_, _) => await ViewModel.LoadAsync();
    }

    private async void OnGpuChosen(object sender, SelectionChangedEventArgs e)
    {
        // The list is rebuilt whenever the settings change, which also lands here; only a
        // choice that differs from the setting is sent.
        if (GpuBox.SelectedItem is GpuChoice choice && ViewModel.Loaded && !ViewModel.IsBusy)
        {
            await ViewModel.SetRenderGpuAsync(choice.Value);
        }
    }

    private async void OnTouchGesturesToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetTouchGesturesOffAsync(TouchGesturesSwitch.IsChecked == true);

    /// <summary>Opens Windows Settings at its touch page, where the gestures are changed for
    /// every app.</summary>
    private void OnOpenTouchSettings(object sender, RoutedEventArgs e)
    {
        try
        {
            System.Diagnostics.Process.Start(
                new System.Diagnostics.ProcessStartInfo(SettingsViewModel.TouchSettingsUri) { UseShellExecute = true });
        }
        catch (System.ComponentModel.Win32Exception)
        {
            // No Settings app opens that page on this computer, so nothing opens.
        }
    }

    private async void OnRestartWindowManager(object sender, RoutedEventArgs e)
    {
        var confirmed = await Shell.Confirm(App.Text.Get("RestartWindowManagerTitle"),
            App.Text.Get("RestartWindowManagerBody"), App.Text.Get("RestartWindowManagerAction"));
        if (!confirmed)
        {
            return;
        }
        var problem = await Shell.RestartWindowManager();
        if (problem is not null)
        {
            await Shell.Tell(App.Text.Get("RestartWindowManagerFailed"), problem);
        }
    }
}
