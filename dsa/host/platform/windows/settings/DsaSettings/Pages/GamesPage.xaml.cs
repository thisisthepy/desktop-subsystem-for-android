using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using Dsa.Settings.Core;

namespace Dsa.Settings.Pages;

/// <summary>The Games page: game convenience, with "Keep game focus" for each installed app
/// (games first) and a Select all that turns every app on, then every app off.</summary>
public partial class GamesPage : Page
{
    private static GamesViewModel ViewModel => App.Games;

    public GamesPage()
    {
        InitializeComponent();
        DataContext = ViewModel;
        Loaded += async (_, _) => await ViewModel.RefreshAsync();
        // The list and its icons go when the page is left; it is read afresh when shown.
        Unloaded += (_, _) => ViewModel.Release();
    }

    private async void OnRefresh(object sender, RoutedEventArgs e) => await ViewModel.RefreshAsync();

    private async void OnSelectAll(object sender, RoutedEventArgs e) => await ViewModel.SelectAllAsync();

    private async void OnKeepFocusChanged(object sender, RoutedEventArgs e)
    {
        if (sender is ToggleButton { Tag: string package } toggle)
        {
            await ViewModel.SetKeepFocusAsync(package, toggle.IsChecked == true);
        }
    }
}
