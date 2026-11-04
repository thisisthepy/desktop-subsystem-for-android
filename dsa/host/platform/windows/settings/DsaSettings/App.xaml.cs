using System.IO;
using System.Windows;
using Dsa.Settings.Core;

namespace Dsa.Settings;

public partial class App : Application
{
    internal static ITextSource Text { get; } = new ResourceText();
    internal static DsaService Service { get; } =
        new(new ProcessDsaCli(ProcessDsaCli.Locate(AppContext.BaseDirectory)));
    internal static GeneralViewModel General { get; } = new(Service, Text);
    internal static SettingsViewModel Settings { get; } = new(Service, Text);
    internal static MemoryViewModel Memory { get; } = new(Service, Text);
    internal static InstallViewModel Install { get; } = new(Service, Text);
    internal static WidgetsViewModel Widgets { get; } = new(Service, Text);

    /// <summary>The argument that names an APK to install: Explorer opens APK files with it, and
    /// the app then shows the APK install window instead of the settings.</summary>
    internal const string InstallApkArgument = "--install-apk";

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        // The app opens on the settings, or, given an APK, on the APK install window alone.
        var at = Array.IndexOf(e.Args, InstallApkArgument);
        if (at >= 0 && at + 1 < e.Args.Length)
        {
            ApkInstallWindow.Open(e.Args[at + 1]);
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
