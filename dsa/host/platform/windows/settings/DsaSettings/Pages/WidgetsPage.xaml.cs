using System.Threading;
using System.Windows;
using System.Windows.Controls;
using Dsa.Settings.Core;

namespace Dsa.Settings.Pages;

/// <summary>The Widgets page: the user places a widget an app in Android offers on the
/// desktop, or removes one placed. The widgets on offer are grouped by app, each app opened
/// on its own to show its widgets as a grid of cards. While the page is shown it watches the
/// placed widgets, so one placed or removed from its window's menu shows here at once.</summary>
public partial class WidgetsPage : Page
{
    private static WidgetsViewModel ViewModel => App.Widgets;

    /// <summary>How long the page waits to watch again after a watch ended, as when Android
    /// stopped.</summary>
    private static readonly TimeSpan WatchRetry = TimeSpan.FromSeconds(5);

    private CancellationTokenSource? _watch;

    public WidgetsPage()
    {
        InitializeComponent();
        DataContext = ViewModel;
        Loaded += async (_, _) =>
        {
            await ViewModel.RefreshAsync();
            if (_watch is null && IsLoaded)
            {
                _watch = new CancellationTokenSource();
                await ViewModel.WatchAsync(WatchRetry, _watch.Token);
            }
        };
        Unloaded += (_, _) =>
        {
            StopWatching();
            // The previews are images of every widget the user looked at; a page that is not
            // shown keeps none of them.
            ViewModel.ReleasePreviews();
        };
        // The watch runs dsa as a child process; it ends with the app, not after it.
        Dispatcher.ShutdownStarted += (_, _) => StopWatching();
    }

    private void StopWatching()
    {
        _watch?.Cancel();
        _watch?.Dispose();
        _watch = null;
    }

    private async void OnRefresh(object sender, RoutedEventArgs e) => await ViewModel.RefreshAsync();

    private static string? PackageOf(object sender) => (sender as FrameworkElement)?.Tag as string;

    private async void OnAppExpanded(object sender, RoutedEventArgs e)
    {
        // A card's own expanders would bubble here too; only the app's counts.
        if (ReferenceEquals(e.OriginalSource, sender) && PackageOf(sender) is { } package)
        {
            await ViewModel.SetExpandedAsync(package, true);
        }
    }

    private async void OnAppCollapsed(object sender, RoutedEventArgs e)
    {
        if (ReferenceEquals(e.OriginalSource, sender) && PackageOf(sender) is { } package)
        {
            await ViewModel.SetExpandedAsync(package, false);
        }
    }

    private async void OnShowMore(object sender, RoutedEventArgs e)
    {
        if (PackageOf(sender) is { } package)
        {
            await ViewModel.ShowMoreAsync(package);
        }
    }

    private async void OnAdd(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: string provider })
        {
            await ViewModel.AddAsync(provider);
        }
    }

    private async void OnRemove(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: int id })
        {
            await ViewModel.RemoveAsync(id);
        }
    }
}
