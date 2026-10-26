using System.Windows;
using System.Windows.Controls;
using System.Windows.Threading;
using Dsa.Settings.Core;

namespace Dsa.Settings.Pages;

public partial class MemoryPage : Page
{
    private readonly DispatcherTimer _timer = new() { Interval = TimeSpan.FromSeconds(5) };

    private static MemoryViewModel ViewModel => App.Memory;

    public MemoryPage()
    {
        InitializeComponent();
        DataContext = ViewModel;
        _timer.Tick += async (_, _) => await ViewModel.RefreshAsync();
        Loaded += async (_, _) =>
        {
            _timer.Start();
            await ViewModel.RefreshAsync();
        };
        Unloaded += (_, _) => _timer.Stop();
    }

    private async void OnRefresh(object sender, RoutedEventArgs e) => await ViewModel.RefreshAsync();
}
