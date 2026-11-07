using System.Globalization;
using System.Text.RegularExpressions;
using System.Xml.Linq;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>The APK Installer's window is English in every language except its title, which
/// is Korean when the Windows UI is Korean, so no other language mixes
/// into it, and it carries DSA's own parcel icon.</summary>
public class ApkInstallerLanguageTests
{
    private static readonly Regex Hangul = new("[가-힣ㄱ-ㆎ]");

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
        throw new FileNotFoundException(inSettings + " was not found above " + AppContext.BaseDirectory);
    }

    private static XDocument Project() => XDocument.Load(Find("DsaApkInstaller", "DsaApkInstaller.csproj"));

    [Fact]
    public void Fr11_installer_embeds_only_the_english_string_table()
    {
        var project = Project();
        var resources = project.Descendants("EmbeddedResource")
            .Select(e => (string?)e.Attribute("Include") ?? "").ToList();
        Assert.Contains(resources, r => r.EndsWith("Strings.resx", StringComparison.Ordinal));
        Assert.DoesNotContain(resources, r => Regex.IsMatch(r, @"Strings\.[a-zA-Z-]+\.resx$"));
        Assert.Equal("en", (string?)project.Descendants("SatelliteResourceLanguages").Single());
    }

    [Fact]
    public void Fr11_installer_reads_its_texts_in_english_whatever_the_language()
    {
        var source = File.ReadAllText(Find("DsaApkInstaller", "Text.cs"));
        Assert.Contains("CultureInfo.InvariantCulture", source);
        Assert.DoesNotContain("CurrentUICulture", source);
    }

    [Fact]
    public void Fr11_installer_window_has_no_korean_text()
    {
        var folder = Find("DsaApkInstaller");
        var files = Directory.EnumerateFiles(folder, "*.xaml")
            .Concat(Directory.EnumerateFiles(folder, "*.cs"));
        foreach (var file in files)
        {
            Assert.False(Hangul.IsMatch(File.ReadAllText(file)), Path.GetFileName(file) + " has Korean text");
        }
        var english = XDocument.Load(Find("DsaSettings", "Strings.resx"));
        foreach (var data in english.Descendants("data"))
        {
            var value = (string?)data.Element("value") ?? "";
            Assert.False(Hangul.IsMatch(value), (string?)data.Attribute("name") + " has Korean text in English");
        }
    }

    [Theory]
    [InlineData("ko-KR")]
    [InlineData("ko")]
    public void Fr11_installer_title_is_korean_when_the_ui_is_korean(string culture)
    {
        Assert.Equal("APK 설치 관리자", ApkInstallerTitle.For(new CultureInfo(culture)));
    }

    [Theory]
    [InlineData("en-US")]
    [InlineData("en-GB")]
    [InlineData("ja-JP")]
    [InlineData("fr-FR")]
    [InlineData("")]
    public void Fr11_installer_title_is_english_when_the_ui_is_not_korean(string culture)
    {
        Assert.Equal("APK Installer", ApkInstallerTitle.For(new CultureInfo(culture)));
    }

    [Fact]
    public void Fr11_installer_window_takes_its_title_from_the_ui_language()
    {
        // Both places the title shows (the window and the file picker) use the lookup, and
        // nothing in the installer reads the localised caption from the settings app's table.
        foreach (var name in new[] { "InstallerWindow.xaml.cs", "App.xaml.cs" })
        {
            var source = File.ReadAllText(Find("DsaApkInstaller", name));
            Assert.Contains("ApkInstallerTitle.For(CultureInfo.CurrentUICulture)", source);
            Assert.DoesNotContain("ApkWindow_Caption", source);
        }
    }

    [Fact]
    public void Fr11_installer_title_is_the_only_korean_text_it_can_show()
    {
        // The title lookup holds exactly one Korean string, the title itself, so no other
        // text can come out Korean through it.
        var source = File.ReadAllText(Find("DsaSettings.Core", "ApkInstallerTitle.cs"));
        var korean = Regex.Matches(source, "\"([^\"]*)\"")
            .Select(m => m.Groups[1].Value).Where(s => Hangul.IsMatch(s)).ToList();
        Assert.Equal(new[] { "APK 설치 관리자" }, korean);
    }

    [Fact]
    public void Fr11_installer_icon_is_dsas_open_box_drawing()
    {
        var project = Project();
        Assert.Equal(@"Assets\apk-installer.ico", (string?)project.Descendants("ApplicationIcon").Single());
        var ico = File.ReadAllBytes(Find("DsaApkInstaller", "Assets", "apk-installer.ico"));
        var png = File.ReadAllBytes(Find("DsaApkInstaller", "Assets", "apk-installer.png"));
        Assert.True(Contains(ico, png), "the .ico does not hold the 256 px open box image");
        // The drawing it is rendered from: the open cardboard box, not DSA's symbol with a badge
        // and not the earlier parcel with an arrow.
        var master = File.ReadAllText(Find("..", "..", "..", "..", "brand", "apk-installer.svg"));
        Assert.Contains("open cardboard box", master);
        Assert.DoesNotContain("arrow", master);
        Assert.DoesNotContain("<filter", master);
        var window = File.ReadAllText(Find("DsaApkInstaller", "InstallerWindow.xaml.cs"));
        Assert.DoesNotContain("WindowsApps", window);
        Assert.DoesNotContain("AppInstaller", window);
    }

    private static bool Contains(byte[] haystack, byte[] needle)
    {
        for (var i = 0; i + needle.Length <= haystack.Length; i++)
        {
            if (haystack.AsSpan(i, needle.Length).SequenceEqual(needle))
            {
                return true;
            }
        }
        return false;
    }
}
