using Xunit;
using System.Xml.Linq;

namespace Dsa.Settings.Core.Tests;

/// <summary>The settings cards are drawn by templates in App.xaml. A card template that loses
/// its fill or its border leaves the header and the control floating on the page, so the
/// styles must set both from the theme's card brushes and the templates must draw them.</summary>
public class CardStyleTests
{
    private static readonly XNamespace Wpf = "http://schemas.microsoft.com/winfx/2006/xaml/presentation";

    private static readonly XNamespace X = "http://schemas.microsoft.com/winfx/2006/xaml";

    private static XDocument AppXaml() => Xaml("App.xaml");

    private static XDocument Xaml(string file)
    {
        // The tests run from dsa/target/settings/..., outside the source tree, so each folder
        // above is tried both as the settings folder and as the repository's dsa folder.
        string[] relative = { Path.Combine("DsaSettings", file),
            Path.Combine("host", "platform", "windows", "settings", "DsaSettings", file) };
        string? found = null;
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null && found is null; dir = dir.Parent)
        {
            found = relative.Select(r => Path.Combine(dir.FullName, r)).FirstOrDefault(File.Exists);
        }
        Assert.True(found is not null, file + " was not found above " + AppContext.BaseDirectory);
        return XDocument.Load(found!);
    }

    [Theory]
    [InlineData("ui:CardControl")]
    [InlineData("ui:CardAction")]
    public void Fr17_a_settings_card_has_the_card_fill_and_border(string type)
    {
        var style = AppXaml().Descendants(Wpf + "Style")
            .SingleOrDefault(s => (string?)s.Attribute("TargetType") == type);
        Assert.True(style is not null, $"App.xaml has no style for {type}");

        string? Setter(string property) => style!.Elements(Wpf + "Setter")
            .FirstOrDefault(s => (string?)s.Attribute("Property") == property)?.Attribute("Value")?.Value;

        Assert.Equal("{DynamicResource CardBackgroundFillColorDefaultBrush}", Setter("Background"));
        Assert.Equal("{DynamicResource CardStrokeColorDefaultBrush}", Setter("BorderBrush"));
        Assert.Equal("1", Setter("BorderThickness"));

        var border = style!.Descendants(Wpf + "ControlTemplate").Single()
            .Descendants(Wpf + "Border").First();
        Assert.Equal("{TemplateBinding Background}", (string?)border.Attribute("Background"));
        Assert.Equal("{TemplateBinding BorderBrush}", (string?)border.Attribute("BorderBrush"));
        Assert.Equal("{TemplateBinding BorderThickness}", (string?)border.Attribute("BorderThickness"));
        Assert.Equal("4", (string?)border.Attribute("CornerRadius"));
    }

    private static XElement KeyedStyle(string key) => AppXaml().Descendants(Wpf + "Style")
        .Single(s => (string?)s.Attribute(X + "Key") == key);

    private static XElement TypedStyle(string type) => AppXaml().Descendants(Wpf + "Style")
        .Single(s => (string?)s.Attribute("TargetType") == type);

    private static string? Setter(XElement style, string property) => style.Elements(Wpf + "Setter")
        .FirstOrDefault(s => (string?)s.Attribute("Property") == property)?.Attribute("Value")?.Value;

    private static string? Resource(string key) => AppXaml().Root!.Descendants()
        .FirstOrDefault(e => (string?)e.Attribute(X + "Key") == key && !e.HasElements)?.Value;

    [Theory]
    // The WinUI type ramp: Title, Subtitle, Body Strong, Body, Caption.
    [InlineData("PageTitle", "28", "36", "SemiBold")]
    [InlineData("SubtitleText", "20", "28", "SemiBold")]
    [InlineData("SectionHeader", "14", "20", "SemiBold")]
    [InlineData("CardTitle", "14", "20", "Normal")]
    [InlineData("CardDescription", "12", "16", "Normal")]
    public void Fr17_text_follows_the_windows_type_ramp(string key, string size, string line, string weight)
    {
        var style = KeyedStyle(key);
        Assert.Equal(size, Setter(style, "FontSize"));
        Assert.Equal(line, Setter(style, "LineHeight"));
        Assert.Equal(weight, Setter(style, "FontWeight"));
    }

    [Fact]
    public void Fr17_a_section_header_sits_30_px_below_the_cards_above_it()
    {
        Assert.Equal("1,30,0,6", Setter(KeyedStyle("SectionHeader"), "Margin"));
    }

    [Theory]
    [InlineData("ui:CardControl")]
    [InlineData("ui:CardAction")]
    public void Fr17_a_settings_card_has_the_toolkit_metrics(string type)
    {
        var style = TypedStyle(type);
        Assert.Equal("16", Setter(style, "Padding"));
        Assert.Equal("68", Setter(style, "MinHeight"));
        var icon = style.Descendants(Wpf + "ContentControl").Single(e => (string?)e.Attribute(X + "Name") == "ControlIcon");
        Assert.Equal("20", (string?)icon.Attribute("FontSize"));
        Assert.Equal("2,0,20,0", (string?)icon.Attribute("Margin"));
    }

    [Fact]
    public void Fr17_a_card_control_keeps_120_px_for_its_control()
    {
        var presenter = TypedStyle("ui:CardControl").Descendants(Wpf + "ContentPresenter")
            .Single(e => (string?)e.Attribute(X + "Name") == "ContentPresenter");
        Assert.Equal("120", (string?)presenter.Attribute("MinWidth"));
        Assert.Equal("24,0,0,0", (string?)presenter.Attribute("Margin"));
    }

    [Fact]
    public void Fr17_a_clickable_card_has_a_13_px_chevron_14_px_after_the_header()
    {
        var chevron = TypedStyle("ui:CardAction").Descendants().Single(e => (string?)e.Attribute(X + "Name") == "ChevronIcon");
        Assert.Equal("13", (string?)chevron.Attribute("FontSize"));
        Assert.Equal("14,0,0,0", (string?)chevron.Attribute("Margin"));
    }

    [Fact]
    public void Fr17_buttons_are_at_least_120_px_wide()
    {
        Assert.Equal("120", Setter(TypedStyle("ui:Button"), "MinWidth"));
    }

    [Fact]
    public void Fr17_text_boxes_and_combo_boxes_are_32_px_high()
    {
        Assert.Equal("32", Resource("TextControlThemeMinHeight"));
    }

    [Fact]
    public void Fr17_a_toggle_switch_puts_on_and_off_left_of_the_switch()
    {
        var template = TypedStyle("ui:ToggleSwitch").Descendants(Wpf + "ControlTemplate").Single();
        var text = template.Descendants(Wpf + "ContentPresenter").Single(e => (string?)e.Attribute(X + "Name") == "ContentPresenter");
        Assert.Equal("0", (string?)text.Attribute("Grid.Column"));
        Assert.Equal("0,0,12,0", (string?)text.Attribute("Margin"));
    }

    [Fact]
    public void Fr17_pages_are_at_most_1000_px_wide_and_36_px_from_the_edges_by_default()
    {
        Assert.Equal("1000", Resource("PageMaxWidth"));
        Assert.Equal("36,16,36,20", Resource("PageHeaderMargin"));
        Assert.Equal("36,0,36,24", Resource("PageContentMargin"));
        Assert.Equal("{DynamicResource PageContentMargin}", Setter(KeyedStyle("PageContent"), "Margin"));
        Assert.Equal("{DynamicResource PageHeaderMargin}", Setter(KeyedStyle("PageHeader"), "Margin"));
    }

    [Fact]
    public void Fr17_navigation_matches_the_settings_app()
    {
        Assert.Equal("40", Setter(TypedStyle("ui:NavigationViewItem"), "MinHeight"));
        XNamespace ui = "http://schemas.lepo.co/wpfui/2022/xaml";
        var window = Xaml("MainWindow.xaml");
        var nav = window.Descendants(ui + "NavigationView").Single();
        Assert.Equal("320", (string?)nav.Attribute("OpenPaneLength"));
        Assert.Equal("Auto", (string?)nav.Attribute("IsBackButtonVisible"));
        Assert.Equal("Mica", (string?)window.Root!.Attribute("WindowBackdropType"));
        Assert.Equal("48", (string?)window.Descendants(ui + "TitleBar").Single().Attribute("Height"));
    }
}
