using System.Xml.Linq;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>The APK Installer is laid out as App Installer lays out an app package. These hold
/// the metrics that make it look so: the Windows type ramp, the spacing, Mica, the icon tile at
/// the top right and the buttons at the bottom right.</summary>
public class ApkInstallerStyleTests
{
    private static readonly XNamespace Wpf = "http://schemas.microsoft.com/winfx/2006/xaml/presentation";
    private static readonly XNamespace X = "http://schemas.microsoft.com/winfx/2006/xaml";
    private static readonly XNamespace Ui = "http://schemas.lepo.co/wpfui/2022/xaml";

    private static XDocument Xaml(string file)
    {
        // The tests run from dsa/target/settings/..., outside the source tree, so each folder
        // above is tried both as the settings folder and as the repository's dsa folder.
        string[] relative = { Path.Combine("DsaApkInstaller", file),
            Path.Combine("host", "platform", "windows", "settings", "DsaApkInstaller", file) };
        string? found = null;
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null && found is null; dir = dir.Parent)
        {
            found = relative.Select(r => Path.Combine(dir.FullName, r)).FirstOrDefault(File.Exists);
        }
        Assert.True(found is not null, file + " was not found above " + AppContext.BaseDirectory);
        return XDocument.Load(found!);
    }

    private static XDocument AppXaml() => Xaml("App.xaml");

    private static XDocument Window() => Xaml("InstallerWindow.xaml");

    private static XElement Named(XDocument doc, string name) =>
        doc.Descendants().Single(e => (string?)e.Attribute(X + "Name") == name);

    private static XElement KeyedStyle(string key) => AppXaml().Descendants(Wpf + "Style")
        .Single(s => (string?)s.Attribute(X + "Key") == key);

    private static string? Setter(XElement style, string property) => style.Elements(Wpf + "Setter")
        .FirstOrDefault(s => (string?)s.Attribute("Property") == property)?.Attribute("Value")?.Value;

    private static string? Resource(string key) => AppXaml().Root!.Descendants()
        .FirstOrDefault(e => (string?)e.Attribute(X + "Key") == key && !e.HasElements)?.Value;

    [Theory]
    // The WinUI type ramp: Title for the app's name, Body Strong, Body and Caption.
    [InlineData("AppNameText", "28", "36", "Normal", "{DynamicResource StrongDisplayFont}")]
    [InlineData("SectionHeader", "14", "20", "Normal", "{DynamicResource StrongTextFont}")]
    [InlineData("DetailLine", "14", "20", "Normal", "{StaticResource TextFont}")]
    [InlineData("BodyText", "14", "20", "Normal", "{StaticResource TextFont}")]
    [InlineData("CaptionText", "12", "16", "Normal", "{StaticResource TextFont}")]
    public void Fr11_installer_text_follows_the_windows_type_ramp(string key, string size, string line, string weight, string font)
    {
        var style = KeyedStyle(key);
        Assert.Equal(size, Setter(style, "FontSize"));
        Assert.Equal(line, Setter(style, "LineHeight"));
        Assert.Equal(weight, Setter(style, "FontWeight"));
        Assert.Equal(font, Setter(style, "FontFamily"));
    }

    [Fact]
    public void Fr11_installer_fonts_are_segoe_ui_variable_with_korean_fallback()
    {
        Assert.Equal("Segoe UI Variable Display, Segoe UI, Malgun Gothic", Resource("DisplayFont"));
        Assert.Equal("Segoe UI Variable Text, Segoe UI, Malgun Gothic", Resource("TextFont"));
    }

    [Fact]
    public void Fr11_installer_keeps_app_installers_spacing()
    {
        Assert.Equal("32,12,32,0", Resource("ContentMargin"));
        Assert.Equal("32,20,32,24", Resource("FooterPadding"));
        Assert.Equal("106", Resource("AppIconTileSize"));
        Assert.Equal("84", Resource("AppIconSize"));
        Assert.Equal("120", Resource("PrimaryButtonMinWidth"));
        Assert.Equal("0,20,0,8", Setter(KeyedStyle("SectionHeader"), "Margin"));
    }

    [Fact]
    public void Fr11_installer_window_has_mica_and_its_own_title_bar()
    {
        var root = Window().Root!;
        Assert.Equal(Ui + "FluentWindow", root.Name);
        Assert.Equal("Mica", (string?)root.Attribute("WindowBackdropType"));
        Assert.Equal("True", (string?)root.Attribute("ExtendsContentIntoTitleBar"));
        Assert.Equal("Round", (string?)root.Attribute("WindowCornerPreference"));
        // The installer tests find the window by this id.
        Assert.Equal("ApkInstallWindow", (string?)root.Attribute(XName.Get("AutomationProperties.AutomationId")));
        var bar = Window().Descendants(Ui + "TitleBar").Single();
        Assert.Equal("False", (string?)bar.Attribute("ShowMaximize"));
        // Only the title's words: no icon in the title bar.
        Assert.Empty(bar.Descendants());
    }

    [Fact]
    public void Fr11_the_app_name_is_large_at_the_top_left_and_the_icon_at_the_top_right()
    {
        var doc = Window();
        var header = Named(doc, "Header");
        var left = header.Elements(Wpf + "StackPanel").Single();
        Assert.Equal("0", (string?)left.Attribute("Grid.Column"));
        Assert.Equal("HeroTitle", (string?)left.Elements(Wpf + "TextBlock").Single().Attribute(X + "Name"));
        var rows = left.Elements(Wpf + "Grid").Single();
        var lines = rows.Elements(Wpf + "TextBlock").Select(t => (string?)t.Attribute(X + "Name")).ToList();
        Assert.Equal(new[] { "PackageLabel", "HeroPackage", "PublisherLabel", "PublisherText", "VersionLabel", "VersionChip",
            "SourceLabel", "SourceText" }, lines);
        Assert.Equal("{StaticResource AppNameText}", (string?)Named(doc, "HeroTitle").Attribute("Style"));
        Assert.Equal("{StaticResource DetailLine}", (string?)Named(doc, "PublisherText").Attribute("Style"));
        Assert.Equal("{StaticResource DetailLine}", (string?)Named(doc, "VersionChip").Attribute("Style"));
        Assert.Equal("{StaticResource DetailLine}", (string?)Named(doc, "SourceText").Attribute("Style"));
        // The lines are in the normal text colour.
        Assert.Equal("{DynamicResource TextFillColorPrimaryBrush}", Setter(KeyedStyle("DetailLine"), "Foreground"));
        var tile = Named(doc, "AppIconTile");
        Assert.Equal("1", (string?)tile.Attribute("Grid.Column"));
        Assert.Equal("Top", (string?)tile.Attribute("VerticalAlignment"));
        Assert.Equal("{StaticResource AppIconTileSize}", (string?)tile.Attribute("Width"));
        Assert.Equal("{StaticResource AppIconTileBrush}", (string?)tile.Attribute("Background"));
        Assert.NotNull(tile.Attribute("CornerRadius"));
        var icon = Named(doc, "AppIcon");
        Assert.Equal(tile, icon.Parent);
        Assert.Equal("{StaticResource AppIconSize}", (string?)icon.Attribute("Width"));
        var clip = icon.Descendants(Wpf + "RectangleGeometry").Single();
        Assert.Equal("12", (string?)clip.Attribute("RadiusX"));
    }

    [Fact]
    public void Fr11_the_lines_come_in_order_then_capabilities_then_notices()
    {
        var doc = Window();
        var names = doc.Descendants().Select(e => (string?)e.Attribute(X + "Name")).Where(n => n is not null).ToList();
        string[] order = { "HeroTitle", "HeroPackage", "PublisherText", "VersionChip", "SourceText", "AppIconTile",
            "PermissionsHeader", "PermissionPreview", "MoreLink", "ExpandedPanel", "SummaryBox", "CompatibilityBox", "LaunchWhenReady" };
        Assert.Equal(order, order.OrderBy(n => names.IndexOf(n)));
    }

    [Theory]
    [InlineData("SummaryBox", "SummaryBadge", "SummaryTitle", "SummaryText")]
    [InlineData("CompatibilityBox", "CompatibilityBadge", "CompatibilityTitle", "CompatibilityText")]
    public void Fr11_a_notice_has_its_icon_on_the_title_line_and_its_text_in_one_column(string box, string badge, string title, string text)
    {
        var doc = Window();
        Assert.Equal("{StaticResource NoticeBox}", (string?)Named(doc, box).Attribute("Style"));
        var slot = Named(doc, badge).Parent!;
        Assert.Equal("20", (string?)slot.Attribute("Height")); // the title's line height
        Assert.Equal("Top", (string?)slot.Attribute("VerticalAlignment"));
        Assert.Equal("Center", (string?)Named(doc, badge).Attribute("VerticalAlignment"));
        Assert.Equal("20", Setter(KeyedStyle("NoticeTitle").Document!.Descendants(Wpf + "Style")
            .Single(s => (string?)s.Attribute(X + "Key") == "SectionHeader"), "LineHeight"));
        Assert.Equal(Named(doc, title).Parent, Named(doc, text).Parent);
        Assert.Equal("1", (string?)Named(doc, title).Parent!.Attribute("Grid.Column"));
    }

    [Fact]
    public void Fr11_there_is_no_divider_band_or_section_header()
    {
        var doc = Window();
        var footer = Named(doc, "Footer");
        Assert.Null(footer.Attribute("BorderThickness"));
        Assert.Null(footer.Attribute("Background"));
        var details = Named(doc, "DetailsPanel");
        Assert.DoesNotContain(details.Descendants(), e => ((string?)e.Attribute("Style"))?.Contains("SectionHeader") == true);
        Assert.Empty(doc.Descendants(Wpf + "Separator"));
        Assert.DoesNotContain(doc.Descendants(), e => (string?)e.Attribute(X + "Name") == "DetailsHeader");
    }

    [Fact]
    public void Fr11_more_expands_in_a_scrolling_area_inside_the_window()
    {
        var doc = Window();
        var scroller = Named(doc, "DetailsScroller");
        Assert.Equal(Wpf + "ScrollViewer", scroller.Name);
        Assert.Equal("Auto", (string?)scroller.Attribute("VerticalScrollBarVisibility"));
        var inside = scroller.Descendants().Select(e => (string?)e.Attribute(X + "Name")).ToList();
        Assert.Contains("PermissionList", inside);
        Assert.Contains("SensitiveChip", inside);
        Assert.Contains("FactList", inside);
        Assert.Equal("OnMore", (string?)Named(doc, "MoreLink").Attribute("Click"));
    }

    [Fact]
    public void Fr11_more_and_less_is_one_link_that_stays_under_the_list_in_both_states()
    {
        // The link sat inside the folded list's panel, which hid with the list it opened, so
        // nothing could fold the list again.
        var doc = Window();
        var link = Named(doc, "MoreLink");
        Assert.Equal(XNamespace.Get("clr-namespace:Dsa.ApkInstaller") + "ExpandLink", link.Name);
        Assert.Single(doc.Descendants(), e => (string?)e.Attribute(X + "Name") == "MoreLink");
        foreach (var hidden in new[] { "PermissionPreview", "PermissionList", "ExpandedPanel" })
        {
            Assert.DoesNotContain(link, Named(doc, hidden).Descendants());
        }
        var names = Named(doc, "DetailsScroller").Descendants().Select(e => (string?)e.Attribute(X + "Name"))
            .Where(n => n is not null).ToList();
        Assert.True(names.IndexOf("PermissionList") < names.IndexOf("MoreLink"));
        Assert.True(names.IndexOf("MoreLink") < names.IndexOf("ExpandedPanel"));
    }

    [Fact]
    public void Fr11_the_lines_under_the_title_sit_tight_with_their_values_in_one_column()
    {
        // Each line was a 20 px line with 4 px added above it, and its height could grow with
        // the font's own line box, so the rows stood far apart.
        var line = KeyedStyle("DetailLine");
        Assert.Equal("20", Setter(line, "LineHeight"));
        Assert.Equal("BlockLineHeight", Setter(line, "LineStackingStrategy"));
        Assert.Equal("0", Setter(line, "Margin"));
        Assert.Null(Setter(line, "MinHeight"));
        var label = KeyedStyle("DetailLabel");
        Assert.Equal("{StaticResource DetailLine}", (string?)label.Attribute("BasedOn"));
        Assert.Equal("0,0,8,0", Setter(label, "Margin"));
        var rows = Named(Window(), "DetailRows");
        Assert.Equal(new[] { "Auto", "*" },
            rows.Descendants(Wpf + "ColumnDefinition").Select(c => (string?)c.Attribute("Width")));
        Assert.All(rows.Descendants(Wpf + "RowDefinition"), r => Assert.Equal("Auto", (string?)r.Attribute("Height")));
        foreach (var text in rows.Elements(Wpf + "TextBlock"))
        {
            Assert.Null(text.Attribute("Margin"));
            var name = (string?)text.Attribute(X + "Name");
            Assert.Equal(name!.EndsWith("Label") ? "{StaticResource DetailLabel}" : "{StaticResource DetailLine}",
                (string?)text.Attribute("Style"));
            Assert.Equal(name.EndsWith("Label") ? null : "1", (string?)text.Attribute("Grid.Column"));
        }
    }

    [Fact]
    public void Fr11_capabilities_come_before_the_details()
    {
        var doc = Window();
        var names = Named(doc, "DetailsPanel").Descendants()
            .Select(e => (string?)e.Attribute(X + "Name")).Where(n => n is not null).ToList();
        Assert.True(names.IndexOf("PermissionsHeader") < names.IndexOf("PermissionList"));
        Assert.True(names.IndexOf("PermissionList") < names.IndexOf("FactList"));
        Assert.Equal("{StaticResource BodyText}", (string?)Named(doc, "PermissionsHeader").Attribute("Style"));
    }

    [Fact]
    public void Fr11_launch_when_ready_sits_left_and_the_main_button_right_in_the_footer()
    {
        var doc = Window();
        var check = Named(doc, "LaunchWhenReady");
        Assert.Equal(Wpf + "CheckBox", check.Name);
        Assert.Equal("0", (string?)check.Attribute("Grid.Column"));
        Assert.Equal("True", (string?)check.Attribute("IsChecked"));
        // The progress takes the checkbox's place while the app installs.
        Assert.Equal("0", (string?)Named(doc, "ProgressPanel").Attribute("Grid.Column"));
        var buttons = Named(doc, "Buttons");
        Assert.Equal("1", (string?)buttons.Attribute("Grid.Column"));
        Assert.Equal("Right", (string?)buttons.Attribute("HorizontalAlignment"));
        // The main buttons come first, in the accent colour, and all have one width.
        Assert.Equal(Wpf + "UniformGrid", buttons.Name);
        Assert.Equal("1", (string?)buttons.Attribute("Rows"));
        var order = buttons.Elements().Select(b => (string?)b.Attribute(X + "Name")).ToList();
        Assert.Equal(new[] { "InstallButton", "OpenButton", "CancelButton", "CloseButton" }, order);
        foreach (var name in new[] { "InstallButton", "OpenButton" })
        {
            Assert.Equal("Primary", (string?)Named(doc, name).Attribute("Appearance"));
        }
        foreach (var button in buttons.Elements())
        {
            Assert.Equal("{StaticResource PrimaryButtonMinWidth}", (string?)button.Attribute("MinWidth"));
        }
        Assert.Equal("{StaticResource FooterPadding}", (string?)Named(doc, "Footer").Attribute("Padding"));
    }
}
