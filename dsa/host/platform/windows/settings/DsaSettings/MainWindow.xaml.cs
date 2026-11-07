using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using Dsa.Settings.Pages;
using Microsoft.Win32;
using Wpf.Ui.Appearance;
using Wpf.Ui.Controls;

namespace Dsa.Settings;

public partial class MainWindow : FluentWindow
{
    private static readonly Dictionary<string, Type> Pages = new(StringComparer.OrdinalIgnoreCase)
    {
        ["general"] = typeof(GeneralPage),
        ["memory"] = typeof(MemoryPage),
        ["apps"] = typeof(AppsPage),
        ["background"] = typeof(BackgroundPage),
        ["display"] = typeof(DisplayPage),
        ["updates"] = typeof(UpdatesPage),
        ["network"] = typeof(NetworkPage),
        ["advanced"] = typeof(AdvancedPage),
        ["device"] = typeof(DeviceIdentityPage),
        ["about"] = typeof(AboutPage),
        ["widgets"] = typeof(WidgetsPage),
        ["logs"] = typeof(LogsPage),
    };

    public MainWindow()
    {
        App.Settings.IsAdministrator = Shell.IsAdministrator();
        InitializeComponent();
        InstallBar.DataContext = App.Install;
        App.Install.PropertyChanged += (_, _) => InstallBar.Severity = App.Install.Severity switch
        {
            Dsa.Settings.Core.InstallSeverity.Success => InfoBarSeverity.Success,
            Dsa.Settings.Core.InstallSeverity.Error => InfoBarSeverity.Error,
            _ => InfoBarSeverity.Informational,
        };
        // The light or the dark theme Windows uses for apps, its accent colour, and their
        // changes while the app runs.
        SystemThemeWatcher.Watch(this, WindowBackdropType.Mica, true);
        // Pages sit 56 px from the edges in a wide window and 36 px in a narrow one, as the
        // Settings app's do; 1008 px is WinUI's wide breakpoint.
        SizeChanged += (_, e) => ApplyPageMargins(e.NewSize.Width);
        Loaded += (_, _) =>
        {
            // The watcher follows changes; the theme to start with is the apps' setting, which
            // it does not read itself.
            ApplicationThemeManager.Apply(AppsUseDarkTheme() ? ApplicationTheme.Dark : ApplicationTheme.Light,
                WindowBackdropType.Mica, true);
            Navigation.Navigate(StartPage());
            MakePaneScroll();
            _ = App.Settings.LoadAsync();
        };
    }

    private const double WideWindowWidth = 1008;

    private void ApplyPageMargins(double width)
    {
        var side = width >= WideWindowWidth ? 56 : 36;
        System.Windows.Application.Current.Resources["PageHeaderMargin"] = new System.Windows.Thickness(side, 16, side, 20);
        System.Windows.Application.Current.Resources["PageContentMargin"] = new System.Windows.Thickness(side, 0, side, 24);
    }

    private static string[]? DroppedFiles(System.Windows.DragEventArgs e) =>
        e.Data.GetDataPresent(System.Windows.DataFormats.FileDrop)
            ? e.Data.GetData(System.Windows.DataFormats.FileDrop) as string[]
            : null;

    /// <summary>Shows a drop is welcome only while it holds an APK and no install runs.</summary>
    private void OnDragOver(object sender, System.Windows.DragEventArgs e)
    {
        e.Effects = App.Install.Accepts(DroppedFiles(e))
            ? System.Windows.DragDropEffects.Copy
            : System.Windows.DragDropEffects.None;
        e.Handled = true;
    }

    private async void OnDrop(object sender, System.Windows.DragEventArgs e)
    {
        if (DroppedFiles(e) is { } files)
        {
            e.Handled = true;
            // Each APK opens in the APK Installer, as it does from Explorer; the bar names any
            // other file, which is not installed.
            foreach (var apk in files.Where(Dsa.Settings.Core.InstallViewModel.IsApk))
            {
                if (Shell.OpenApkInstaller(apk) is { } why)
                {
                    await Shell.Tell(App.Text.Get("ApkWindow_Caption"), why);
                }
            }
            var others = files.Where(f => !Dsa.Settings.Core.InstallViewModel.IsApk(f)).ToList();
            if (others.Count > 0)
            {
                await App.Install.DropAsync(others);
            }
        }
    }

    /// <summary>Lets the navigation pane scroll when the window is too short for every item:
    /// by the mouse wheel anywhere over the pane, and by moving the keyboard focus, which
    /// brings the focused item into view. The pane's scroll viewer in the library's template
    /// scrolls by item and lets the items swallow the wheel, so a short window left the last
    /// items out of reach.</summary>
    private void MakePaneScroll()
    {
        // The library's items take no keyboard focus, so Tab and the arrow keys skipped the
        // pane. Each item takes the focus, and Enter or Space opens its page.
        foreach (var item in Navigation.MenuItems.OfType<NavigationViewItem>()
                     .Concat(Navigation.FooterMenuItems.OfType<NavigationViewItem>()))
        {
            item.Focusable = true;
            item.IsTabStop = true;
            item.KeyDown += (_, e) =>
            {
                if ((e.Key == Key.Enter || e.Key == Key.Space) && item.TargetPageType is { } page)
                {
                    Navigation.Navigate(page);
                    e.Handled = true;
                }
            };
        }
        Navigation.ApplyTemplate();
        if (Navigation.Template?.FindName("PaneGrid", Navigation) is not FrameworkElement pane
            || Descendant<ScrollViewer>(pane) is not { } scroller)
        {
            return;
        }
        scroller.CanContentScroll = false;
        scroller.VerticalScrollBarVisibility = ScrollBarVisibility.Auto;
        scroller.HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled;
        scroller.PanningMode = PanningMode.VerticalOnly;
        pane.PreviewMouseWheel += (_, e) =>
        {
            if (scroller.ScrollableHeight <= 0)
            {
                return;
            }
            scroller.ScrollToVerticalOffset(scroller.VerticalOffset - e.Delta / 3.0);
            e.Handled = true;
        };
        // The focus can land on an item or on a part inside it; the whole item comes into view,
        // once the layout has placed it.
        Navigation.AddHandler(Keyboard.GotKeyboardFocusEvent, new KeyboardFocusChangedEventHandler((_, e) =>
        {
            if (e.NewFocus is not DependencyObject focused || !scroller.IsAncestorOf(focused))
            {
                return;
            }
            var item = focused;
            while (item is not null and not NavigationViewItem)
            {
                item = VisualTreeHelper.GetParent(item);
            }
            if ((item ?? focused) is FrameworkElement shown)
            {
                Dispatcher.BeginInvoke(() => ScrollIntoView(scroller, shown),
                    System.Windows.Threading.DispatcherPriority.Loaded);
            }
        }), true);
    }

    /// <summary>Scrolls <paramref name="scroller"/> just enough to show all of
    /// <paramref name="element"/>.</summary>
    private static void ScrollIntoView(ScrollViewer scroller, FrameworkElement element)
    {
        var top = element.TransformToAncestor(scroller).Transform(new Point(0, 0)).Y;
        var bottom = top + element.ActualHeight;
        if (top < 0)
        {
            scroller.ScrollToVerticalOffset(scroller.VerticalOffset + top);
        }
        else if (bottom > scroller.ViewportHeight)
        {
            scroller.ScrollToVerticalOffset(scroller.VerticalOffset + bottom - scroller.ViewportHeight);
        }
    }

    /// <summary>The first element of type <typeparamref name="T"/> under <paramref name="root"/>.</summary>
    private static T? Descendant<T>(DependencyObject root) where T : DependencyObject
    {
        for (var i = 0; i < VisualTreeHelper.GetChildrenCount(root); i++)
        {
            var child = VisualTreeHelper.GetChild(root, i);
            if (child is T found)
            {
                return found;
            }
            if (Descendant<T>(child) is { } deeper)
            {
                return deeper;
            }
        }
        return null;
    }

    /// <summary>Whether Windows has apps use the dark theme.</summary>
    private static bool AppsUseDarkTheme()
    {
        using var key = Registry.CurrentUser.OpenSubKey(
            @"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
        return key?.GetValue("AppsUseLightTheme") is int light && light == 0;
    }

    /// <summary>The page to open with: <c>--page memory</c> (general, memory, background,
    /// display, updates, network, advanced, device, about, widgets or logs) opens another than General.</summary>
    private static Type StartPage()
    {
        var args = Environment.GetCommandLineArgs();
        for (var i = 1; i + 1 < args.Length; i++)
        {
            if (args[i] == "--page" && Pages.TryGetValue(args[i + 1], out var page))
            {
                return page;
            }
        }
        return typeof(GeneralPage);
    }
}
