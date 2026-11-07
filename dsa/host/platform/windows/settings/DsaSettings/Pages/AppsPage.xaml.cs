using System.Globalization;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using Dsa.Settings.Core;

namespace Dsa.Settings.Pages;

/// <summary>The Apps page: every app installed in Android, to find, open, see in Android's own
/// settings (permissions, storage, notifications) or uninstall; and Android's own list of
/// apps.</summary>
public partial class AppsPage : Page
{
    private static AppsViewModel ViewModel => App.Apps;

    public AppsPage()
    {
        InitializeComponent();
        DataContext = ViewModel;
        Loaded += async (_, _) =>
        {
            await ViewModel.RefreshAsync();
            await ViewModel.LoadWindowSettingsAsync();
        };
    }

    private async void OnResizable(object sender, RoutedEventArgs e)
    {
        if (sender is ToggleButton { Tag: string package } toggle)
        {
            await ViewModel.SetResizableAsync(package, toggle.IsChecked == true);
        }
    }

    private async void OnRunAsArm(object sender, RoutedEventArgs e)
    {
        if (sender is ToggleButton { Tag: string package } toggle)
        {
            await ViewModel.SetRunAsArmAsync(package, toggle.IsChecked == true, label => Shell.Confirm(
                string.Format(CultureInfo.CurrentCulture, App.Text.Get("AppsRunAsArmEraseTitle"), label),
                string.Format(CultureInfo.CurrentCulture, App.Text.Get("AppsRunAsArmEraseBody"), label),
                App.Text.Get("AppsRunAsArmEraseButton")));
        }
    }

    private async void OnStartSize(object sender, SelectionChangedEventArgs e)
    {
        // The list takes the app's size when it fills in; only a choice the user makes counts.
        var chosen = e.RemovedItems.Count > 0 || sender is ComboBox { IsDropDownOpen: true }
            || sender is ComboBox { IsKeyboardFocusWithin: true };
        if (sender is ComboBox { Tag: string package, SelectedIndex: >= 0 } box && chosen)
        {
            await ViewModel.SetStartSizeAsync(package, box.SelectedIndex);
        }
    }

    private static string? PackageOf(object sender) =>
        sender is FrameworkElement { Tag: string package } ? package : null;

    private async void OnRefresh(object sender, RoutedEventArgs e) => await ViewModel.RefreshAsync();

    private async void OnManage(object sender, RoutedEventArgs e) => await ViewModel.ManageAsync();

    private async void OnOpen(object sender, RoutedEventArgs e)
    {
        if (PackageOf(sender) is { } package)
        {
            await ViewModel.OpenAsync(package);
        }
    }

    private async void OnInfo(object sender, RoutedEventArgs e)
    {
        if (PackageOf(sender) is { } package)
        {
            await ViewModel.InfoAsync(package);
        }
    }

    private async void OnUninstall(object sender, RoutedEventArgs e)
    {
        if (PackageOf(sender) is not { } package)
        {
            return;
        }
        var label = ViewModel.LabelOf(package);
        var confirmed = await Shell.Confirm(
            string.Format(CultureInfo.CurrentCulture, App.Text.Get("AppsUninstallConfirmTitle"), label),
            string.Format(CultureInfo.CurrentCulture, App.Text.Get("AppsUninstallConfirmBody"), label),
            App.Text.Get("AppsUninstallButton.Content"));
        if (confirmed)
        {
            await ViewModel.UninstallAsync(package);
        }
    }
}
