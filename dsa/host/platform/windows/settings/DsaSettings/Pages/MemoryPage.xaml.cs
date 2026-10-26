using System.ComponentModel;
using Dsa.Settings.Core;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Dsa.Settings.Pages;

public sealed partial class MemoryPage : Page, INotifyPropertyChanged
{
    private readonly DispatcherTimer _timer = new() { Interval = TimeSpan.FromSeconds(5) };

    public event PropertyChangedEventHandler? PropertyChanged;

    internal MemoryViewModel ViewModel => App.Memory;

    /// <summary>Whether no host process of the user's Android is running.</summary>
    internal bool HostNotRunning => ViewModel.Report is not null && !ViewModel.HostRunning;

    public MemoryPage()
    {
        InitializeComponent();
        ViewModel.PropertyChanged += OnViewModelChanged;
        _timer.Tick += async (_, _) => await ViewModel.RefreshAsync();
        Loaded += async (_, _) =>
        {
            _timer.Start();
            await ViewModel.RefreshAsync();
        };
        Unloaded += (_, _) =>
        {
            _timer.Stop();
            ViewModel.PropertyChanged -= OnViewModelChanged;
        };
    }

    private void OnViewModelChanged(object? sender, PropertyChangedEventArgs e) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(HostNotRunning)));

    private async void OnRefresh(object sender, RoutedEventArgs e) => await ViewModel.RefreshAsync();
}
