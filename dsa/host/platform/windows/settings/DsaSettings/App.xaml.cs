using System.IO;
using System.Windows;
using Dsa.Settings.Core;

namespace Dsa.Settings;

public partial class App : Application
{
    internal static ITextSource Text { get; } = new ResourceText();
    private static readonly ProcessDsaCli Cli = new(ProcessDsaCli.Locate(AppContext.BaseDirectory));
    internal static DsaService Service { get; } = new(Cli);
    // Each page's view model is made when it is first used, not all of them at start: a
    // window that shows one page holds that page's state alone. The UI thread is the only
    // one that asks, so the first use makes it once.
    private static LogViewModel? _logs;
    private static GeneralViewModel? _general;
    private static SettingsViewModel? _settings;
    private static ProgramUpdateViewModel? _programUpdate;
    private static MemoryViewModel? _memory;
    private static InstallViewModel? _install;
    private static AppsViewModel? _apps;
    private static WidgetsViewModel? _widgets;
    private static DeviceIdentityViewModel? _deviceIdentity;
    private static BackgroundViewModel? _background;
    private static AndroidVersionViewModel? _androidVersion;
    internal static LogViewModel Logs => _logs ??= new(Cli, Service, Text);
    internal static GeneralViewModel General => _general ??= new(Service, Text);
    internal static SettingsViewModel Settings => _settings ??= new(Service, Text);
    internal static ProgramUpdateViewModel ProgramUpdate => _programUpdate ??= new(Service, Text);
    internal static MemoryViewModel Memory => _memory ??= new(Service, Text);
    internal static InstallViewModel Install => _install ??= new(Service, Text);
    internal static AppsViewModel Apps => _apps ??= new(new AppsService(Cli), Text);
    internal static WidgetsViewModel Widgets => _widgets ??= new(Service, Text, Cli);
    internal static DeviceIdentityViewModel DeviceIdentity => _deviceIdentity ??= new(Cli, Service, Text);
    internal static BackgroundViewModel Background => _background ??= new(Service, Text);
    internal static AndroidVersionViewModel AndroidVersion => _androidVersion ??= new(Cli, Text);

    /// <summary>The argument that names an APK to install. Earlier releases associated APK files
    /// with the settings app and this argument; the app now hands such an APK on to the APK
    /// Installer, its own program beside it, and shows no settings.</summary>
    internal const string InstallApkArgument = "--install-apk";

    /// <summary>The argument that names an app to uninstall: the UninstallString of each
    /// Android app in Windows' list of installed apps runs the app with it.</summary>
    internal const string UninstallAppArgument = "--uninstall-app";

    static App()
    {
        // The app draws in software, set before anything of WPF's media layer runs: before the
        // application's resources load and before its first window exists. Hardware rendering
        // creates a Direct3D 9 device in the graphics driver's user mode part, which keeps its
        // own heaps and a back buffer for every window: tens of MiB of private memory for a
        // window of text, switches and lists, which software rendering draws as fast as anyone
        // can see.
        System.Windows.Media.RenderOptions.ProcessRenderMode = System.Windows.Interop.RenderMode.SoftwareOnly;
    }

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        General.Updating = () => ProgramUpdate.InProgress;
        // Each app's "Follow the default" says where the Display page's choice takes it now.
        Apps.Follow(Settings);
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
        // Japanese and Chinese are drawn in their own UI fonts, not in Malgun Gothic's Korean
        // forms of the same characters.
        var language = UiLanguage.Resolve(System.Globalization.CultureInfo.CurrentUICulture);
        if (UiFonts.For(language) is { } fonts)
        {
            Resources["TextFont"] = new System.Windows.Media.FontFamily(fonts.Text);
            Resources["DisplayFont"] = new System.Windows.Media.FontFamily(fonts.Display);
            Resources["StrongTextFont"] = new System.Windows.Media.FontFamily(fonts.StrongText);
            Resources["StrongDisplayFont"] = new System.Windows.Media.FontFamily(fonts.StrongDisplay);
        }
        // Malgun Gothic, Microsoft YaHei UI and Microsoft JhengHei UI have no Semibold: a
        // strong font would draw Korean and Chinese titles in Bold.
        if (UiFonts.HasNoSemibold(language))
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
