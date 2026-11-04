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
        LimitSection.DataContext = App.Settings;
        _timer.Tick += async (_, _) => await ViewModel.RefreshAsync();
        Loaded += async (_, _) =>
        {
            _timer.Start();
            await ViewModel.RefreshAsync();
            await App.Settings.LoadAsync();
        };
        Unloaded += (_, _) => _timer.Stop();
    }

    private async void OnRefresh(object sender, RoutedEventArgs e) => await ViewModel.RefreshAsync();

    private async void OnLimitChanged(object sender, SelectionChangedEventArgs e)
    {
        // The list is rebuilt whenever the settings change, which also lands here; only a
        // choice that differs from the setting is sent.
        if (LimitBox.SelectedItem is MemoryLimitChoice choice && App.Settings.Loaded && !App.Settings.IsBusy)
        {
            await App.Settings.SetMemoryLimitAsync(choice.Mb);
        }
    }
}
