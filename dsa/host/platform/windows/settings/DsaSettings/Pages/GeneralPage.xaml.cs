using Dsa.Settings.Core;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Dsa.Settings.Pages;

public sealed partial class GeneralPage : Page
{
    private readonly DispatcherTimer _timer = new() { Interval = TimeSpan.FromSeconds(3) };

    internal GeneralViewModel ViewModel => App.General;

    public GeneralPage()
    {
        InitializeComponent();
        _timer.Tick += async (_, _) => await ViewModel.PollAsync();
        Loaded += async (_, _) =>
        {
            _timer.Start();
            await ViewModel.RefreshAsync();
        };
        Unloaded += (_, _) => _timer.Stop();
    }

    private async void OnStart(object sender, RoutedEventArgs e) => await ViewModel.StartAsync();

    private async void OnStop(object sender, RoutedEventArgs e) => await ViewModel.StopAsync();

    private async void OnRefresh(object sender, RoutedEventArgs e) => await ViewModel.RefreshAsync();

    private void OnOpenLogs(object sender, RoutedEventArgs e) => Shell.OpenFolder(Shell.UserLogFolder, XamlRoot);
}
