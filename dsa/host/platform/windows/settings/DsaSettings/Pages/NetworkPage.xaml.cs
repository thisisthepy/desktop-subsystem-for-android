using System.Windows;
using System.Windows.Controls;
using System.Windows.Threading;
using Dsa.Settings.Core;

namespace Dsa.Settings.Pages;

/// <summary>How Android reaches the network, the root certificates added to this PC that
/// Android trusts, and developer mode: how adb reaches Android through this PC's loopback
/// address.</summary>
public partial class NetworkPage : Page
{
    /// <summary>Reads where adb auto-connect stands while the page is shown.</summary>
    private readonly DispatcherTimer _adbTimer = new() { Interval = TimeSpan.FromSeconds(3) };

    private static SettingsViewModel ViewModel => App.Settings;

    public NetworkPage()
    {
        InitializeComponent();
        DataContext = ViewModel;
        _adbTimer.Tick += async (_, _) => await ViewModel.PollAdbAutoConnectAsync();
        Loaded += async (_, _) =>
        {
            _adbTimer.Start();
            await ViewModel.LoadAsync();
            await ViewModel.RefreshNetworkAsync();
        };
        Unloaded += (_, _) => _adbTimer.Stop();
    }

    private async void OnRefresh(object sender, RoutedEventArgs e) => await ViewModel.RefreshNetworkAsync();

    private async void OnAdvancedNetworkingToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetAdvancedNetworkingAsync(AdvancedNetworkingSwitch.IsChecked == true);

    private async void OnHostCertificatesToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetHostCertificatesAsync(HostCertificatesSwitch.IsChecked == true);

    private async void OnDeveloperModeToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetDeveloperModeAsync(DeveloperModeSwitch.IsChecked == true);

    private async void OnAdbAutoConnectToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetAdbAutoConnectAsync(AdbAutoConnectSwitch.IsChecked == true);

    /// <summary>Copies the adb command, and shows a check mark on the button for a moment. The
    /// clipboard can be held by another program, and then the button stays as it was.</summary>
    private async void OnCopyAdbCommand(object sender, RoutedEventArgs e)
    {
        try
        {
            System.Windows.Clipboard.SetDataObject(AdbCommand.Text, copy: true);
        }
        catch (System.Runtime.InteropServices.ExternalException)
        {
            return;
        }
        AdbCopyButton.Icon = new Wpf.Ui.Controls.SymbolIcon { Symbol = Wpf.Ui.Controls.SymbolRegular.Checkmark24 };
        await System.Threading.Tasks.Task.Delay(1500);
        AdbCopyButton.Icon = new Wpf.Ui.Controls.SymbolIcon { Symbol = Wpf.Ui.Controls.SymbolRegular.Copy24 };
    }

    private async void OnAdbPortChanged(object sender, RoutedEventArgs e) =>
        await ViewModel.SetAdbPortAsync(AdbPortBox.Text);

    private async void OnAdbPortKeyDown(object sender, System.Windows.Input.KeyEventArgs e)
    {
        if (e.Key == System.Windows.Input.Key.Enter)
        {
            await ViewModel.SetAdbPortAsync(AdbPortBox.Text);
        }
    }
}
