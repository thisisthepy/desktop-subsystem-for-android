using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using Dsa.Settings.Core;

namespace Dsa.Settings.Pages;

/// <summary>The "Running in the background" page: the apps in Android that keep running with
/// no window, which the user opens or stops, and whether a stop from the notification area
/// asks first.</summary>
public partial class BackgroundPage : Page
{
    private static BackgroundViewModel ViewModel => App.Background;

    public BackgroundPage()
    {
        InitializeComponent();
        DataContext = ViewModel;
        Loaded += async (_, _) => await ViewModel.RefreshAsync();
        // The list and its icons go when the page is left; it is read afresh when shown.
        Unloaded += (_, _) => ViewModel.Release();
    }

    private async void OnRefresh(object sender, RoutedEventArgs e) => await ViewModel.RefreshAsync();

    private async void OnOpen(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: string package })
        {
            await ViewModel.OpenAsync(package);
        }
    }

    private async void OnStop(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: string package })
        {
            await ViewModel.StopAsync(package);
        }
    }

    private async void OnAskChanged(object sender, RoutedEventArgs e)
    {
        if (sender is ToggleButton toggle)
        {
            await ViewModel.SetAskBeforeStopAsync(toggle.IsChecked == true);
        }
    }
}
