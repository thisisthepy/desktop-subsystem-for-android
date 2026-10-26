using Dsa.Settings.Core;
using Microsoft.UI.Xaml;
using Microsoft.Windows.ApplicationModel.Resources;

namespace Dsa.Settings;

/// <summary>The texts in the user's language, from Strings\&lt;language&gt;\Resources.resw.
/// Windows picks the language from the user's display language.</summary>
internal sealed class ResourceText : ITextSource
{
    private readonly ResourceLoader _loader = new();

    public string Get(string key)
    {
        try
        {
            var value = _loader.GetString(key);
            return string.IsNullOrEmpty(value) ? key : value;
        }
        catch (Exception)
        {
            return key;
        }
    }
}

public partial class App : Application
{
    private Window? _window;

    internal static ITextSource Text { get; } = new ResourceText();
    internal static DsaService Service { get; } =
        new(new ProcessDsaCli(ProcessDsaCli.Locate(AppContext.BaseDirectory)));
    internal static GeneralViewModel General { get; } = new(Service, Text);
    internal static SettingsViewModel Settings { get; } = new(Service, Text);
    internal static MemoryViewModel Memory { get; } = new(Service, Text);

    /// <summary>The folder DSA is installed in: the settings app's folder is inside it.</summary>
    internal static string InstallFolder =>
        Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, ".."));

    public App()
    {
        InitializeComponent();
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        _window = new MainWindow();
        _window.Activate();
    }
}
