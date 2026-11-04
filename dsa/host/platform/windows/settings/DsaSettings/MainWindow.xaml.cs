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
        ["display"] = typeof(DisplayPage),
        ["updates"] = typeof(UpdatesPage),
        ["advanced"] = typeof(AdvancedPage),
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
            // Each APK opens in the APK install window, as it does from Explorer; the bar names
            // any other file, which is not installed.
            foreach (var apk in files.Where(Dsa.Settings.Core.InstallViewModel.IsApk))
            {
                ApkInstallWindow.Open(apk);
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
        pane.AddHandler(UIElement.GotKeyboardFocusEvent, new KeyboardFocusChangedEventHandler((_, e) =>
        {
            if (e.NewFocus is FrameworkElement focused)
            {
                focused.BringIntoView();
            }
        }), true);
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

    /// <summary>The page to open with: <c>--page memory</c> (general, memory, display,
    /// updates, advanced, about, widgets or logs) opens another than General.</summary>
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
