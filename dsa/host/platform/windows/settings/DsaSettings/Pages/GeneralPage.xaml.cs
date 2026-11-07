using System.Windows;
using System.Windows.Controls;
using System.Windows.Threading;
using Dsa.Settings.Core;

namespace Dsa.Settings.Pages;

public partial class GeneralPage : Page
{
    private readonly DispatcherTimer _timer = new() { Interval = TimeSpan.FromSeconds(3) };

    private static GeneralViewModel ViewModel => App.General;

    public GeneralPage()
    {
        InitializeComponent();
        DataContext = ViewModel;
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

    /// <summary>Copies what the service reported. The clipboard can be held by another program,
    /// and then nothing is copied.</summary>
    private void OnCopyLastReason(object sender, RoutedEventArgs e)
    {
        try
        {
            System.Windows.Clipboard.SetDataObject(ViewModel.LastReasonDetail, copy: true);
        }
        catch (System.Runtime.InteropServices.ExternalException)
        {
        }
    }

    private async void OnFreeze(object sender, RoutedEventArgs e) => await ViewModel.FreezeAsync();

    private async void OnRefresh(object sender, RoutedEventArgs e) => await ViewModel.RefreshAsync();

    private void OnOpenLogs(object sender, RoutedEventArgs e) => Shell.OpenFolder(Shell.UserLogFolder);

    private void OnOpenServiceLogs(object sender, RoutedEventArgs e) =>
        Shell.OpenFolder(Shell.ServiceLogFolder(App.Settings.LogDir));
}
