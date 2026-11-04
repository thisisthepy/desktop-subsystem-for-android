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

    private async void OnInputTraceToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetInputTraceAsync(InputTraceSwitch.IsChecked == true);

    private async void OnResizeTraceToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetResizeTraceAsync(ResizeTraceSwitch.IsChecked == true);

    private void OnInputTraceAsAdministrator(object sender, RoutedEventArgs e) =>
        ChangeAsAdministrator("input-trace", !ViewModel.InputTrace);

    private void OnResizeTraceAsAdministrator(object sender, RoutedEventArgs e) =>
        ChangeAsAdministrator("resize-trace", !ViewModel.ResizeTrace);

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

    private void OnOpenUserLogs(object sender, RoutedEventArgs e) => Shell.OpenFolder(Shell.UserLogFolder);

    private void OnOpenServiceLogs(object sender, RoutedEventArgs e) => Shell.OpenFolder(Shell.ServiceLogFolder(ViewModel.LogDir));
}
