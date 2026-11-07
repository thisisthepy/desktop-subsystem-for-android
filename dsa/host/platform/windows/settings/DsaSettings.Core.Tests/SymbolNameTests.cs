using System.Reflection;
using System.Reflection.Metadata;
using System.Reflection.PortableExecutable;
using System.Text.RegularExpressions;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>Every icon the settings app and the APK Installer name in XAML or code exists in
/// WPF UI 3.0.5. XAML turns the name into the library's symbol when the element is made, so a
/// name the library lacks fails the page that holds it the moment it opens: the Advanced page
/// with "Store24" threw before it showed.</summary>
public class SymbolNameTests
{
    private static HashSet<string> LibrarySymbols()
    {
        var path = typeof(SymbolNameTests).Assembly.GetCustomAttributes<AssemblyMetadataAttribute>()
            .Single(a => a.Key == "WpfUiAssembly").Value!;
        Assert.True(File.Exists(path), "WPF UI 3.0.5 was not found at " + path);
        using var stream = File.OpenRead(path);
        using var pe = new PEReader(stream);
        var metadata = pe.GetMetadataReader();
        var symbols = metadata.TypeDefinitions.Select(metadata.GetTypeDefinition)
            .Single(t => metadata.GetString(t.Name) == "SymbolRegular"
                && metadata.GetString(t.Namespace) == "Wpf.Ui.Controls");
        return symbols.GetFields().Select(metadata.GetFieldDefinition)
            .Where(f => (f.Attributes & FieldAttributes.Static) != 0)
            .Select(f => metadata.GetString(f.Name))
            .ToHashSet();
    }

    private static string SettingsRoot()
    {
        // The tests run from dsa/target/settings/..., outside the source tree, so each folder
        // above is tried both as the settings folder and as the repository's dsa folder.
        string[] relative = { "DsaSettings", Path.Combine("host", "platform", "windows", "settings", "DsaSettings") };
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null; dir = dir.Parent)
        {
            var found = relative.Select(r => Path.Combine(dir.FullName, r)).FirstOrDefault(Directory.Exists);
            if (found is not null)
            {
                return Path.GetDirectoryName(found)!;
            }
        }
        Assert.Fail("the settings app's source was not found above " + AppContext.BaseDirectory);
        return "";
    }

    [Fact]
    public void Fr17_every_icon_the_apps_name_is_a_wpf_ui_symbol()
    {
        var symbols = LibrarySymbols();
        Assert.Contains("Delete24", symbols);
        Assert.DoesNotContain("Store24", symbols);

        var root = SettingsRoot();
        var name = new Regex(@"\{ui:SymbolIcon\s+(?:Symbol=)?(\w+)|\bSymbol=""(\w+)""|SymbolRegular\.(\w+)");
        var missing = new[] { "DsaSettings", "DsaApkInstaller" }
            .SelectMany(app => Directory.GetFiles(Path.Combine(root, app), "*.*", SearchOption.AllDirectories))
            .Where(file => file.EndsWith(".xaml", StringComparison.Ordinal) || file.EndsWith(".cs", StringComparison.Ordinal))
            .Where(file => !file.Contains(Path.DirectorySeparatorChar + "obj" + Path.DirectorySeparatorChar)
                && !file.Contains(Path.DirectorySeparatorChar + "bin" + Path.DirectorySeparatorChar))
            .SelectMany(file => name.Matches(File.ReadAllText(file))
                .Select(m => (file: Path.GetFileName(file),
                    symbol: m.Groups.Values.Skip(1).First(g => g.Success).Value)))
            .Where(use => !symbols.Contains(use.symbol))
            .Select(use => $"{use.file}: {use.symbol}")
            .Distinct()
            .ToList();
        Assert.True(missing.Count == 0,
            "these icons are not in WPF UI 3.0.5, so the element that names them fails when it is made: "
            + string.Join(", ", missing));
    }
}
