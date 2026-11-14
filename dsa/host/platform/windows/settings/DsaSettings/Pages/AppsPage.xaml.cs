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
            ShowHighlighted();
        };
    }

    /// <summary>Brings the row of the app a task window's App info named into view and gives its
    /// first button the keyboard focus, once the list has laid it out, so the user lands on the
    /// app's settings and a screen reader reads its name.</summary>
    private void ShowHighlighted()
    {
        if (ViewModel.Highlighted is not { } package)
        {
            return;
        }
        Dispatcher.BeginInvoke(() =>
        {
            var index = ViewModel.Rows.ToList().FindIndex(row => row.Package == package);
            if (index < 0 || AppList.ItemContainerGenerator.ContainerFromIndex(index) is not FrameworkElement row)
            {
                return;
            }
            row.BringIntoView();
            if (FirstFocusable(row) is { } first)
            {
                first.Focus();
            }
        }, System.Windows.Threading.DispatcherPriority.Loaded);
    }

    /// <summary>The first element under <paramref name="root"/> that takes the keyboard focus.</summary>
    private static UIElement? FirstFocusable(DependencyObject root)
    {
        for (var i = 0; i < System.Windows.Media.VisualTreeHelper.GetChildrenCount(root); i++)
        {
            var child = System.Windows.Media.VisualTreeHelper.GetChild(root, i);
            if (child is ButtonBase { Focusable: true, IsEnabled: true } button)
            {
                return button;
            }
            if (FirstFocusable(child) is { } deeper)
            {
                return deeper;
            }
        }
        return null;
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

    private async void OnDpiScaling(object sender, SelectionChangedEventArgs e)
    {
        // As with the start size, only a choice the user makes counts, not the list filling in.
        var chosen = e.RemovedItems.Count > 0 || sender is ComboBox { IsDropDownOpen: true }
            || sender is ComboBox { IsKeyboardFocusWithin: true };
        if (sender is ComboBox { Tag: string package, SelectedIndex: >= 0 } box && chosen)
        {
            await ViewModel.SetDpiScalingAsync(package, box.SelectedIndex);
        }
    }

    private async void OnRoot(object sender, SelectionChangedEventArgs e)
    {
        // As with the start size, only a choice the user makes counts, not the list filling in.
        var chosen = e.RemovedItems.Count > 0 || sender is ComboBox { IsDropDownOpen: true }
            || sender is ComboBox { IsKeyboardFocusWithin: true };
        if (sender is ComboBox { Tag: string package, SelectedIndex: >= 0 } box && chosen)
        {
            await ViewModel.SetRootAsync(package, box.SelectedIndex);
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

    private async void OnDesktopShortcut(object sender, RoutedEventArgs e)
    {
        if (PackageOf(sender) is { } package)
        {
            await ViewModel.CreateDesktopShortcutAsync(package);
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
