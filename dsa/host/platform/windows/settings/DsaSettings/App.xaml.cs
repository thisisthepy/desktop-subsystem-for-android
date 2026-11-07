using System.IO;
using System.Windows;
using Dsa.Settings.Core;

namespace Dsa.Settings;

public partial class App : Application
{
    internal static ITextSource Text { get; } = new ResourceText();
    private static readonly ProcessDsaCli Cli = new(ProcessDsaCli.Locate(AppContext.BaseDirectory));
    internal static DsaService Service { get; } = new(Cli);
    internal static LogViewModel Logs { get; } = new(Cli, Service, Text);
    internal static GeneralViewModel General { get; } = new(Service, Text);
    internal static SettingsViewModel Settings { get; } = new(Service, Text);
    internal static MemoryViewModel Memory { get; } = new(Service, Text);
    internal static InstallViewModel Install { get; } = new(Service, Text);
    internal static AppsViewModel Apps { get; } = new(new AppsService(Cli), Text);
    internal static WidgetsViewModel Widgets { get; } = new(Service, Text, Cli);
    internal static DeviceIdentityViewModel DeviceIdentity { get; } = new(Cli, Service, Text);
    internal static BackgroundViewModel Background { get; } = new(Service, Text);
    internal static AndroidVersionViewModel AndroidVersion { get; } = new(Cli, Text);

    /// <summary>The argument that names an APK to install. Earlier releases associated APK files
    /// with the settings app and this argument; the app now hands such an APK on to the APK
    /// Installer, its own program beside it, and shows no settings.</summary>
    internal const string InstallApkArgument = "--install-apk";

    /// <summary>The argument that names an app to uninstall: the UninstallString of each
    /// Android app in Windows' list of installed apps runs the app with it.</summary>
    internal const string UninstallAppArgument = "--uninstall-app";

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        // `--lang ko-KR` shows the app in another language than the user's display language,
        // for pictures of each language taken on one machine.
        var lang = Array.IndexOf(e.Args, "--lang");
        if (lang >= 0 && lang + 1 < e.Args.Length)
        {
            try
            {
                var culture = System.Globalization.CultureInfo.GetCultureInfo(e.Args[lang + 1]);
                System.Globalization.CultureInfo.DefaultThreadCurrentUICulture = culture;
                System.Globalization.CultureInfo.CurrentUICulture = culture;
            }
            catch (System.Globalization.CultureNotFoundException)
            {
            }
        }
        // Malgun Gothic has no Semibold: a strong font would draw Korean titles in Bold.
        if (System.Globalization.CultureInfo.CurrentUICulture.TwoLetterISOLanguageName == "ko")
        {
            Resources["StrongTextFont"] = Resources["TextFont"];
            Resources["StrongDisplayFont"] = Resources["DisplayFont"];
        }
        // The text beside every toggle switch, in the language just chosen.
        Resources["ToggleOnText"] = Text.Get("Toggle.On");
        Resources["ToggleOffText"] = Text.Get("Toggle.Off");
        // The app opens on the settings, or, given an APK, hands it to the APK Installer.
        var at = Array.IndexOf(e.Args, InstallApkArgument);
        if (at >= 0 && at + 1 < e.Args.Length)
        {
            if (Shell.OpenApkInstaller(e.Args[at + 1]) is { } why)
            {
                System.Windows.MessageBox.Show(why, Text.Get("ApkWindow_Caption"), MessageBoxButton.OK, MessageBoxImage.Error);
            }
            Shutdown();
            return;
        }
        // Windows' list of installed apps opens the uninstall window alone, with no console.
        var uninstall = Array.IndexOf(e.Args, UninstallAppArgument);
        if (uninstall >= 0 && uninstall + 1 < e.Args.Length)
        {
            UninstallAppWindow.Open(e.Args[uninstall + 1]);
            return;
        }
        new MainWindow().Show();
    }

    /// <summary>The folder DSA is installed in: the settings app's folder is inside it.</summary>
    internal static string InstallFolder =>
        Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, ".."));

    /// <summary>The machine's DSA folder the installer recorded, or null when it recorded
    /// none.</summary>
    internal static string? DataFolder
    {
        get
        {
            try
            {
                using var key = Microsoft.Win32.RegistryKey
                    .OpenBaseKey(Microsoft.Win32.RegistryHive.LocalMachine, Microsoft.Win32.RegistryView.Registry64)
                    .OpenSubKey(@"SOFTWARE\thisisthepy\DSA");
                return (key?.GetValue("DataFolder") as string)?.TrimEnd('\\');
            }
            catch (Exception e) when (e is System.Security.SecurityException or UnauthorizedAccessException or IOException)
            {
                return null;
            }
        }
    }
}
