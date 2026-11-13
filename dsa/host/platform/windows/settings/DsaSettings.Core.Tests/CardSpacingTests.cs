using System.Xml.Linq;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>Every card in a section sits the same distance from its neighbours, with the same
/// padding inside. The spacing used to be written on each card, and a closed notice bar kept the
/// 4 px its page gave it: in the Widevine section two closed bars put 12 px between the Widevine
/// card and the cards under it, where every other gap is 4 px, and a card and two bars set their
/// own spacing (0,16,0,0, 0,8,0,0 and 0,0,0,8). Now the styles in App.xaml own the padding, the
/// height and the spacing, and a page that sets its own fails here.</summary>
public class CardSpacingTests
{
    private static readonly XNamespace Wpf = "http://schemas.microsoft.com/winfx/2006/xaml/presentation";

    private static readonly XNamespace X = "http://schemas.microsoft.com/winfx/2006/xaml";

    private static readonly XNamespace Ui = "http://schemas.lepo.co/wpfui/2022/xaml";

    /// <summary>The kinds of box a settings page stacks: cards, a card that opens, notice bars.</summary>
    private static readonly string[] Boxes = { "CardControl", "CardAction", "CardExpander", "InfoBar" };

    /// <summary>What only the shared style may set on such a box.</summary>
    private static readonly string[] Owned = { "Margin", "Padding", "MinHeight", "Height", "MaxHeight", "Style", "Template" };

    private static string SettingsApp()
    {
        // The tests run from dsa/target/settings/..., outside the source tree, so each folder
        // above is tried both as the settings folder and as the repository's dsa folder.
        string[] relative = { "DsaSettings", Path.Combine("host", "platform", "windows", "settings", "DsaSettings") };
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null; dir = dir.Parent)
        {
            var found = relative.Select(r => Path.Combine(dir.FullName, r))
                .FirstOrDefault(d => File.Exists(Path.Combine(d, "App.xaml")) && File.Exists(Path.Combine(d, "DsaSettings.csproj")));
            if (found is not null)
            {
                return found;
            }
        }
        Assert.Fail("the settings app's source was not found above " + AppContext.BaseDirectory);
        return "";
    }

    private static XDocument AppXaml() => XDocument.Load(Path.Combine(SettingsApp(), "App.xaml"));

    /// <summary>Every XAML file of the settings app but App.xaml, which holds the styles.</summary>
    private static IEnumerable<(string file, XDocument doc)> Views() =>
        Directory.GetFiles(SettingsApp(), "*.xaml", SearchOption.AllDirectories)
            .Where(file => !file.Contains(Path.DirectorySeparatorChar + "obj" + Path.DirectorySeparatorChar)
                && !file.Contains(Path.DirectorySeparatorChar + "bin" + Path.DirectorySeparatorChar)
                && Path.GetFileName(file) != "App.xaml")
            .Select(file => (Path.GetFileName(file), XDocument.Load(file)));

    private static XElement TypedStyle(string type) => AppXaml().Descendants(Wpf + "Style")
        .Single(s => (string?)s.Attribute("TargetType") == type && s.Attribute(X + "Key") is null);

    private static string? Setter(XElement style, string property) => style.Elements(Wpf + "Setter")
        .FirstOrDefault(s => (string?)s.Attribute("Property") == property)?.Attribute("Value")?.Value;

    [Fact]
    public void Fr17_cards_are_4_px_apart_by_one_shared_value()
    {
        var spacing = AppXaml().Root!.Descendants(Wpf + "Thickness")
            .SingleOrDefault(e => (string?)e.Attribute(X + "Key") == "CardSpacing");
        Assert.True(spacing is not null, "App.xaml has no CardSpacing");
        Assert.Equal("0,0,0,4", spacing!.Value);
    }

    [Theory]
    [InlineData("ui:CardControl")]
    [InlineData("ui:CardAction")]
    [InlineData("ui:CardExpander")]
    [InlineData("ui:InfoBar")]
    public void Fr17_a_card_or_notice_bar_takes_its_spacing_from_its_style(string type)
    {
        Assert.Equal("{StaticResource CardSpacing}", Setter(TypedStyle(type), "Margin"));
    }

    [Fact]
    public void Fr17_a_card_expander_is_based_on_the_library_style()
    {
        Assert.Equal("{StaticResource DefaultUiCardExpanderStyle}", (string?)TypedStyle("ui:CardExpander").Attribute("BasedOn"));
    }

    [Fact]
    public void Fr17_a_closed_notice_bar_takes_no_room()
    {
        // Collapsing the template alone keeps the bar's margin, so the trigger must be the
        // style's and must collapse the bar itself.
        var trigger = TypedStyle("ui:InfoBar").Elements(Wpf + "Style.Triggers").Elements(Wpf + "Trigger")
            .SingleOrDefault(t => (string?)t.Attribute("Property") == "IsOpen" && (string?)t.Attribute("Value") == "False");
        Assert.True(trigger is not null, "the info bar style has no IsOpen=False trigger of its own");
        var setter = trigger!.Elements(Wpf + "Setter").SingleOrDefault(s => (string?)s.Attribute("Property") == "Visibility");
        Assert.True(setter is not null && setter.Attribute("TargetName") is null && (string?)setter.Attribute("Value") == "Collapsed",
            "a closed info bar must collapse itself, not only its template");
    }

    [Fact]
    public void Fr17_no_page_sets_a_card_or_notice_bar_spacing_padding_or_height_of_its_own()
    {
        var views = Views().ToList();
        Assert.Contains(views, v => v.file == "AdvancedPage.xaml");
        var offenders = new List<string>();
        foreach (var (file, doc) in views)
        {
            foreach (var box in doc.Descendants().Where(e => e.Name.Namespace == Ui && Boxes.Contains(e.Name.LocalName)))
            {
                // The install bar is the window's, above the page, and lines up with the page
                // header through the header's own margin resource.
                if (file == "MainWindow.xaml" && (string?)box.Attribute(X + "Name") == "InstallBar")
                {
                    continue;
                }
                foreach (var own in Owned.Where(a => box.Attribute(a) is not null))
                {
                    var name = (string?)box.Attribute(X + "Name") ?? (string?)box.Attribute("Title") ?? (string?)box.Attribute("Icon") ?? "";
                    offenders.Add($"{file}: {box.Name.LocalName} {name} sets {own}=\"{(string?)box.Attribute(own)}\"");
                }
            }
        }
        Assert.True(offenders.Count == 0,
            "these set what the shared card and bar styles own, so they sit out of line with the cards around them: "
            + string.Join("; ", offenders));
    }

    [Fact]
    public void Fr17_an_intro_note_keeps_the_space_before_the_cards_in_its_style()
    {
        var intro = AppXaml().Descendants(Wpf + "Style").Single(s => (string?)s.Attribute(X + "Key") == "PageIntro");
        Assert.Equal("{StaticResource Note}", (string?)intro.Attribute("BasedOn"));
        Assert.Equal("1,6,0,12", Setter(intro, "Margin"));
        var offenders = Views()
            .SelectMany(v => v.doc.Descendants(Wpf + "TextBlock")
                .Where(t => (string?)t.Attribute("Style") == "{StaticResource PageIntro}" && t.Attribute("Margin") is not null)
                .Select(t => $"{v.file}: {(string?)t.Attribute("Text")}"))
            .ToList();
        Assert.True(offenders.Count == 0, "these intro notes set their own margin: " + string.Join("; ", offenders));
    }
}
