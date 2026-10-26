using Dsa.Settings.Core;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Dsa.Settings.Pages;

public sealed partial class AdvancedPage : Page
{
    internal SettingsViewModel ViewModel => App.Settings;

    public AdvancedPage()
    {
        InitializeComponent();
        Loaded += async (_, _) => await ViewModel.LoadAsync();
    }

    private async void OnFreezeToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetFreezeAsync(FreezeSwitch.IsOn);

    private void OnOpenUserLogs(object sender, RoutedEventArgs e) => Shell.OpenFolder(Shell.UserLogFolder, XamlRoot);

    private void OnOpenServiceLogs(object sender, RoutedEventArgs e) => Shell.OpenFolder(ViewModel.LogDir, XamlRoot);
}
