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
    };

    public MainWindow()
    {
        App.Settings.IsAdministrator = Shell.IsAdministrator();
        InitializeComponent();
        // The light or the dark theme Windows uses for apps, its accent colour, and their
        // changes while the app runs.
        ApplicationThemeManager.Apply(AppsUseDarkTheme() ? ApplicationTheme.Dark : ApplicationTheme.Light,
            WindowBackdropType.Mica, true);
        SystemThemeWatcher.Watch(this, WindowBackdropType.Mica, true);
        Loaded += (_, _) =>
        {
            Navigation.Navigate(StartPage());
            _ = App.Settings.LoadAsync();
        };
    }

    /// <summary>Whether Windows has apps use the dark theme.</summary>
    private static bool AppsUseDarkTheme()
    {
        using var key = Registry.CurrentUser.OpenSubKey(
            @"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
        return key?.GetValue("AppsUseLightTheme") is int light && light == 0;
    }

    /// <summary>The page to open with: <c>--page memory</c> (general, memory, display,
    /// updates, advanced or about) opens another than General.</summary>
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
