using System.Reflection;
using Dsa.Settings.Core;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Dsa.Settings.Pages;

public sealed partial class AboutPage : Page
{
    internal SettingsViewModel ViewModel => App.Settings;

    /// <summary>This app's version, as the installer build stamps it.</summary>
    internal string AppVersion =>
        typeof(App).Assembly.GetCustomAttribute<AssemblyInformationalVersionAttribute>()?.InformationalVersion
            .Split('+')[0]
        ?? typeof(App).Assembly.GetName().Version?.ToString()
        ?? "";

    public AboutPage()
    {
        InitializeComponent();
        Loaded += async (_, _) => await ViewModel.LoadAsync();
    }

    private void OnOpenLicenses(object sender, RoutedEventArgs e) => Shell.OpenFolder(Shell.LicenseFolder, XamlRoot);
}
