using System.Xml.Linq;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>The settings app's icon is the plain DSA symbol, the same drawing as DSA's own
/// icon, with no badge in its corner.</summary>
public class SettingsIconTests
{
    private static string Find(params string[] parts)
    {
        // The tests run from dsa/target/settings/..., outside the source tree, so each folder
        // above is tried both as the settings folder and as the repository's dsa folder.
        var inSettings = Path.Combine(parts);
        string[] relative = { inSettings,
            Path.Combine("host", "platform", "windows", "settings", inSettings) };
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null; dir = dir.Parent)
        {
            var found = relative.Select(r => Path.Combine(dir.FullName, r))
                .FirstOrDefault(p => File.Exists(p) || Directory.Exists(p));
            if (found is not null)
            {
                return found;
            }
        }
        throw new FileNotFoundException(inSettings);
    }

    [Fact]
    public void Fr17_settings_icon_has_no_badge()
    {
        var project = XDocument.Load(Find("DsaSettings", "DsaSettings.csproj"));
        Assert.Equal(@"Assets\logo.ico", (string?)project.Descendants("ApplicationIcon").Single());
        foreach (var file in new[] { "logo.ico", "logo.png" })
        {
            var settings = File.ReadAllBytes(Find("DsaSettings", "Assets", file));
            var plain = File.ReadAllBytes(Find("..", "installer", "setup-ui", "assets", file));
            Assert.True(settings.AsSpan().SequenceEqual(plain),
                $"the settings app's {file} is not the plain DSA symbol of the installer's {file}");
        }
    }
}
