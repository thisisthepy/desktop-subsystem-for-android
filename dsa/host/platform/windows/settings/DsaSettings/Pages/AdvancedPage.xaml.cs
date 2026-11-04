using System.Windows;
using System.Windows.Controls;
using Dsa.Settings.Core;

namespace Dsa.Settings.Pages;

public partial class AdvancedPage : Page
{
    private static SettingsViewModel ViewModel => App.Settings;

    public AdvancedPage()
    {
        InitializeComponent();
        DataContext = ViewModel;
        Loaded += async (_, _) => await ViewModel.LoadAsync();
    }

    private async void OnLinksToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetLinksInAndroidAsync(LinksSwitch.IsChecked == true);

    private async void OnClipboardToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetClipboardAsync(ClipboardSwitch.IsChecked == true);

    private async void OnDeveloperModeToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetDeveloperModeAsync(DeveloperModeSwitch.IsChecked == true);

    private async void OnAdbPortChanged(object sender, RoutedEventArgs e) =>
        await ViewModel.SetAdbPortAsync(AdbPortBox.Text);

    private async void OnAdbPortKeyDown(object sender, System.Windows.Input.KeyEventArgs e)
    {
        if (e.Key == System.Windows.Input.Key.Enter)
        {
            await ViewModel.SetAdbPortAsync(AdbPortBox.Text);
        }
    }

    private async void OnFreezeToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetFreezeAsync(FreezeSwitch.IsChecked == true);

    private void OnOpenUserLogs(object sender, RoutedEventArgs e) => Shell.OpenFolder(Shell.UserLogFolder);

    private void OnOpenServiceLogs(object sender, RoutedEventArgs e) => Shell.OpenFolder(Shell.ServiceLogFolder(ViewModel.LogDir));
}
