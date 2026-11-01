using System.Windows;
using System.Windows.Controls;
using Dsa.Settings.Core;

namespace Dsa.Settings.Pages;

/// <summary>The Widgets page: the user places a widget an app in Android offers on the
/// desktop, or removes one placed.</summary>
public partial class WidgetsPage : Page
{
    private static WidgetsViewModel ViewModel => App.Widgets;

    public WidgetsPage()
    {
        InitializeComponent();
        DataContext = ViewModel;
        Loaded += async (_, _) => await ViewModel.RefreshAsync();
    }

    private async void OnRefresh(object sender, RoutedEventArgs e) => await ViewModel.RefreshAsync();

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
