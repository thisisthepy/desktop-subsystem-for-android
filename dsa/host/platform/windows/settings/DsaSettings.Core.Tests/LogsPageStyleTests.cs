using Xunit;
using System.Xml.Linq;

namespace Dsa.Settings.Core.Tests;

/// <summary>Log lines are drawn in the app's own font, like all other text in the app. A
/// monospace family here once made the page look foreign and its Korean text fall to another
/// font.</summary>
public class LogsPageStyleTests
{
    private static readonly XNamespace Wpf = "http://schemas.microsoft.com/winfx/2006/xaml/presentation";

    private static XDocument Page()
    {
        // The tests run from dsa/target/settings/..., outside the source tree, so each folder
        // above is tried both as the settings folder and as the repository's dsa folder.
        string[] relative = { Path.Combine("DsaSettings", "Pages", "LogsPage.xaml"),
            Path.Combine("host", "platform", "windows", "settings", "DsaSettings", "Pages", "LogsPage.xaml") };
        string? found = null;
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null && found is null; dir = dir.Parent)
        {
            found = relative.Select(r => Path.Combine(dir.FullName, r)).FirstOrDefault(File.Exists);
        }
        Assert.True(found is not null, "LogsPage.xaml was not found above " + AppContext.BaseDirectory);
        return XDocument.Load(found!);
    }

    private static IEnumerable<string> FontFamilies(XDocument page) =>
        page.Descendants().Attributes("FontFamily").Select(a => a.Value)
            .Concat(page.Descendants(Wpf + "Setter")
                .Where(s => (string?)s.Attribute("Property") == "FontFamily")
                .Select(s => (string?)s.Attribute("Value") ?? ""))
            .Concat(page.Descendants(Wpf + "FontFamily").Select(f => f.Value));

    [Fact]
    public void Fr21_the_log_page_sets_no_monospace_font_family()
    {
        string[] monospace = { "Mono", "Consolas", "Courier", "Code", "Lucida Console", "Fixedsys" };
        foreach (var family in FontFamilies(Page()))
        {
            Assert.False(monospace.Any(m => family.Contains(m, StringComparison.OrdinalIgnoreCase)),
                $"LogsPage.xaml sets the font family \"{family}\"; log lines use the app's TextFont");
        }
    }

    [Fact]
    public void Fr21_log_lines_select_several_and_copy_with_ctrl_c_and_the_menu()
    {
        var list = Page().Descendants(Wpf + "ListBox").Single();
        Assert.Equal("Extended", (string?)list.Attribute("SelectionMode"));
        Assert.Contains(list.Descendants(Wpf + "CommandBinding"),
            b => (string?)b.Attribute("Command") == "ApplicationCommands.Copy");
        Assert.Contains(list.Descendants(Wpf + "MenuItem"),
            m => (string?)m.Attribute("Command") == "ApplicationCommands.Copy");
    }

    [Fact]
    public void Fr21_a_log_row_uses_the_body_font_on_a_line_that_fits_korean_text()
    {
        var cell = Page().Descendants(Wpf + "Style")
            .Single(s => (string?)s.Attribute("TargetType") == "TextBlock");
        string? Setter(string property) => cell.Elements(Wpf + "Setter")
            .FirstOrDefault(s => (string?)s.Attribute("Property") == property)?.Attribute("Value")?.Value;

        Assert.Equal("{StaticResource TextFont}", Setter("FontFamily"));
        Assert.Equal("14", Setter("FontSize"));
        // Malgun Gothic needs about 18.6 px at 14 px; a fixed line also keeps every row one height.
        Assert.Equal("20", Setter("LineHeight"));
        Assert.Equal("BlockLineHeight", Setter("LineStackingStrategy"));
    }
}
