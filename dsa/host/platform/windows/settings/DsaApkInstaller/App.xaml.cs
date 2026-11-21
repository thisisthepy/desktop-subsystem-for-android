using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows;
using Dsa.Settings.Core;

namespace Dsa.ApkInstaller;

public partial class App : Application
{
    internal static ITextSource Text { get; } = new ResourceText();

    /// <summary>The argument that names the APK to show: the .apk association runs the app with
    /// it, and so do the settings app and the app window host when they hand an APK on.</summary>
    internal const string InstallApkArgument = "--install-apk";

    /// <summary>The app's own identity in the taskbar, apart from the settings app's, so its
    /// windows group under its own name and icon.</summary>
    internal const string AppUserModelId = "Thisisthepy.DSA.ApkInstaller";

    [DllImport("shell32.dll", CharSet = CharSet.Unicode, PreserveSig = true)]
    private static extern int SetCurrentProcessExplicitAppUserModelID(string appId);

    static App()
    {
        // Software rendering, as the settings app, set before the application's resources load
        // and before the first window: hardware rendering creates a Direct3D device in the
        // graphics driver, with private heaps of its own, for one window of text and a
        // progress bar.
        System.Windows.Media.RenderOptions.ProcessRenderMode = System.Windows.Interop.RenderMode.SoftwareOnly;
    }

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        _ = SetCurrentProcessExplicitAppUserModelID(AppUserModelId);
        // `--lang ko-KR` shows the app in another language than the user's display language,
        // for pictures of each language taken on one machine.
        var lang = Array.IndexOf(e.Args, "--lang");
        if (lang >= 0 && lang + 1 < e.Args.Length)
        {
            try
            {
                var culture = CultureInfo.GetCultureInfo(e.Args[lang + 1]);
                CultureInfo.DefaultThreadCurrentUICulture = culture;
                CultureInfo.CurrentUICulture = culture;
            }
            catch (CultureNotFoundException)
            {
            }
        }
        // Japanese and Chinese are drawn in their own UI fonts, not in Malgun Gothic's Korean
        // forms of the same characters.
        var language = UiLanguage.Resolve(CultureInfo.CurrentUICulture);
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
        var path = ApkPath(e.Args);
        if (path is null)
        {
            // Started without a file (by hand, or from Open with on nothing): ask for one.
            var picker = new Microsoft.Win32.OpenFileDialog
            {
                Title = Text.Get("ApkWindow_Caption"),
                Filter = PickerFilter(Text),
                CheckFileExists = true,
            };
            if (picker.ShowDialog() != true)
            {
                Shutdown();
                return;
            }
            path = picker.FileName;
        }
        InstallerWindow.Open(path);
    }

    /// <summary>The file picker's two choices, named in the user's language: APKs and split APK
    /// bundles, and all files. The patterns are the same in every language.</summary>
    internal static string PickerFilter(ITextSource text) =>
        $"{text.Get("ApkWindow_PickerApks")} (*.apk, *.xapk, *.apks, *.apkm)|*.apk;*.xapk;*.apks;*.apkm|" +
        $"{text.Get("ApkWindow_PickerAllFiles")} (*.*)|*.*";

    /// <summary>The APK the command line names: after --install-apk, or on its own, as Open
    /// with passes it.</summary>
    internal static string? ApkPath(string[] args)
    {
        var at = Array.IndexOf(args, InstallApkArgument);
        if (at >= 0)
        {
            return at + 1 < args.Length ? args[at + 1] : null;
        }
        for (var i = 0; i < args.Length; i++)
        {
            if (args[i] == "--lang")
            {
                i++;
                continue;
            }
            if (!args[i].StartsWith("--", StringComparison.Ordinal))
            {
                return Path.GetFullPath(args[i]);
            }
        }
        return null;
    }
}
