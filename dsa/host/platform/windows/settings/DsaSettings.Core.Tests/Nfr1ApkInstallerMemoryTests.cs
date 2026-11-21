using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>The APK Installer's own memory, on the settings app's rules: it draws in software,
/// gives its memory back while minimised or hidden, and compacts the heap after a step.</summary>
public class Nfr1ApkInstallerMemoryTests
{
    private static string Read(params string[] parts)
    {
        var inSettings = Path.Combine(parts);
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null; dir = dir.Parent)
        {
            foreach (var root in new[] { dir.FullName, Path.Combine(dir.FullName, "host", "platform", "windows", "settings") })
            {
                var path = Path.Combine(root, inSettings);
                if (File.Exists(path))
                {
                    return File.ReadAllText(path);
                }
            }
        }
        throw new FileNotFoundException(inSettings);
    }

    [Fact]
    public void Nfr1_the_apk_installer_renders_in_software_before_its_first_window()
    {
        var app = Read("DsaApkInstaller", "App.xaml.cs");
        var software = app.IndexOf("RenderOptions.ProcessRenderMode = System.Windows.Interop.RenderMode.SoftwareOnly",
            StringComparison.Ordinal);
        Assert.True(software >= 0, "the APK Installer does not set software rendering");
        Assert.True(software < app.IndexOf("base.OnStartup(e)", StringComparison.Ordinal));
        Assert.True(software < app.IndexOf("InstallerWindow.Open(", StringComparison.Ordinal));
    }

    [Fact]
    public void Nfr1_a_minimised_or_hidden_apk_installer_gives_its_memory_back()
    {
        // The settings app's trim, linked in, not a copy.
        Assert.Contains(@"<Compile Include=""..\DsaSettings\WorkingSetTrim.cs""", Read("DsaApkInstaller", "DsaApkInstaller.csproj"));
        Assert.Contains("Dsa.Settings.WorkingSetTrim.Attach(this)", Read("DsaApkInstaller", "InstallerWindow.xaml.cs"));
        Assert.True(MemoryTrim.After(WindowChange.Minimized).EmptyWorkingSet);
    }

    [Fact]
    public void Nfr1_the_apk_installer_compacts_the_heap_after_a_step_is_left()
    {
        var window = Read("DsaApkInstaller", "InstallerWindow.xaml.cs");
        Assert.Contains("_shownPage is { } shown && shown != page", window);
        Assert.Contains("Dsa.Settings.WorkingSetTrim.PageLeft(this)", window);
        Assert.True(MemoryTrim.After(WindowChange.PageLeft).CompactHeap);
    }

    [Fact]
    public void Nfr1_the_apk_installer_sets_software_rendering_before_its_resources_load()
    {
        var app = Read("DsaApkInstaller", "App.xaml.cs");
        var staticCtor = app.IndexOf("static App()", StringComparison.Ordinal);
        Assert.True(staticCtor >= 0, "the software mode is not set in the static constructor");
        var software = app.IndexOf("RenderMode.SoftwareOnly", staticCtor, StringComparison.Ordinal);
        Assert.True(software > staticCtor && software < app.IndexOf("OnStartup(", StringComparison.Ordinal));
    }

    [Fact]
    public void Nfr1_apk_installer_tool_tips_draw_no_shadow_effect()
    {
        var xaml = System.Xml.Linq.XDocument.Parse(Read("DsaApkInstaller", "App.xaml"));
        System.Xml.Linq.XNamespace p = "http://schemas.microsoft.com/winfx/2006/xaml/presentation";
        var toolTip = xaml.Descendants(p + "Style").Single(s => (string?)s.Attribute("TargetType") == "ToolTip");
        Assert.Contains(toolTip.Elements(p + "Setter"), s => (string?)s.Attribute("Property") == "Template");
        Assert.Contains(toolTip.Elements(p + "Setter"),
            s => (string?)s.Attribute("Property") == "Effect" && (string?)s.Attribute("Value") == "{x:Null}");
        Assert.DoesNotContain(xaml.Descendants(), e => e.Name.LocalName.EndsWith("Effect", StringComparison.Ordinal));
    }

    [Fact]
    public void Nfr1_the_apk_installer_runs_with_the_settings_apps_runtime_options()
    {
        var project = System.Xml.Linq.XDocument.Parse(Read("DsaApkInstaller", "DsaApkInstaller.csproj"));
        string? Property(string name) => (string?)project.Descendants(name).SingleOrDefault();
        string? Option(string name) => project.Descendants("RuntimeHostConfigurationOption")
            .Where(o => (string?)o.Attribute("Include") == name)
            .Select(o => (string?)o.Attribute("Value")).SingleOrDefault();
        Assert.Equal("false", Property("ServerGarbageCollection"));
        Assert.Equal("false", Property("ConcurrentGarbageCollection"));
        Assert.Equal("false", Property("TieredPGO"));
        Assert.Equal("false", Property("TieredCompilation"));
        Assert.Equal("5", Option("System.GC.ConserveMemory"));
        Assert.Equal("true", Option("System.Globalization.UseNls"));
        Assert.Equal("true", Option("Switch.System.Windows.Input.Stylus.EnablePointerSupport"));
    }
}
