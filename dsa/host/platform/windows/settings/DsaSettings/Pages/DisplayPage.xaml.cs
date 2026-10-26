using Dsa.Settings.Core;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Dsa.Settings.Pages;

public sealed partial class DisplayPage : Page
{
    internal SettingsViewModel ViewModel => App.Settings;

    public DisplayPage()
    {
        InitializeComponent();
        Loaded += async (_, _) => await ViewModel.LoadAsync();
    }

    private async void OnGpuChosen(object sender, SelectionChangedEventArgs e)
    {
        // The list is rebuilt whenever the settings change, which also lands here; only a
        // choice that differs from the setting is sent.
        if (GpuBox.SelectedItem is GpuChoice choice && ViewModel.Loaded)
        {
            await ViewModel.SetRenderGpuAsync(choice.Value);
        }
    }

    private async void OnRestartWindowManager(object sender, RoutedEventArgs e)
    {
        var confirmed = await Shell.Confirm(XamlRoot, App.Text.Get("RestartWindowManagerTitle"),
            App.Text.Get("RestartWindowManagerBody"), App.Text.Get("RestartWindowManagerAction"));
        if (!confirmed)
        {
            return;
        }
        var problem = await Shell.RestartWindowManager();
        if (problem is not null)
        {
            await Shell.Tell(XamlRoot, App.Text.Get("RestartWindowManagerFailed"), problem);
        }
    }
}
