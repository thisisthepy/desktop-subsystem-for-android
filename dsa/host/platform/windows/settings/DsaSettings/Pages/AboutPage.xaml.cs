using System.Reflection;
using System.Windows;
using System.Windows.Controls;

namespace Dsa.Settings.Pages;

public partial class AboutPage : Page
{
    /// <summary>This app's version, as the installer build stamps it.</summary>
    private static string AppVersion =>
        typeof(App).Assembly.GetCustomAttribute<AssemblyInformationalVersionAttribute>()?.InformationalVersion
            .Split('+')[0]
        ?? typeof(App).Assembly.GetName().Version?.ToString()
        ?? "";

    public AboutPage()
    {
        InitializeComponent();
        DataContext = App.Settings;
        AppVersionText.Text = AppVersion;
        HeaderVersion.Text = AppVersion;
        Loaded += async (_, _) => await App.Settings.LoadAsync();
    }

    private void OnOpenLicenses(object sender, RoutedEventArgs e) => Shell.OpenFolder(Shell.LicenseFolder);
}
