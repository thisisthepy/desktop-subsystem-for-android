using System.Windows;
using Dsa.Settings.Pages;
using Wpf.Ui.Appearance;
using Wpf.Ui.Controls;

namespace Dsa.Settings;

public partial class MainWindow : FluentWindow
{
    public MainWindow()
    {
        // The light or the dark theme and the accent colour Windows uses, and their changes.
        ApplicationThemeManager.ApplySystemTheme(true);
        InitializeComponent();
        SystemThemeWatcher.Watch(this, WindowBackdropType.Mica, true);
        Loaded += (_, _) =>
        {
            Navigation.Navigate(typeof(GeneralPage));
            _ = App.Settings.LoadAsync();
        };
    }
}
