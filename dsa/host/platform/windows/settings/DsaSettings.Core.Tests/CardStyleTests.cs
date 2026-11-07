using System.Reflection;
using System.Text;
using System.Text.RegularExpressions;
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
        Assert.Equal("{StaticResource CardCornerRadius}", (string?)border.Attribute("CornerRadius"));
        Assert.Equal("8", Resource("CardCornerRadius"));
    }

    // A Semibold family name in the list also turns Korean into Bold, so both apps must replace
    // the strong fonts with the regular ones when they run in Korean.
    [Theory]
    [InlineData("DsaSettings")]
    [InlineData("DsaApkInstaller")]
    public void Fr17_korean_replaces_the_strong_fonts_with_the_regular_ones(string app)
    {
        var code = File.ReadAllText(XamlPath(app, "App.xaml.cs"));
        Assert.Contains("TwoLetterISOLanguageName == \"ko\"", code);
        Assert.Contains("Resources[\"StrongTextFont\"] = Resources[\"TextFont\"]", code);
        Assert.Contains("Resources[\"StrongDisplayFont\"] = Resources[\"DisplayFont\"]", code);
    }

    // Malgun Gothic has Regular, Semilight and Bold. A weight of Medium or more on a style turns
    // Korean text into Bold.
    [Theory]
    [InlineData("DsaSettings", "PageTitle")]
    [InlineData("DsaSettings", "SubtitleText")]
    [InlineData("DsaSettings", "SectionHeader")]
    [InlineData("DsaSettings", "CardTitle")]
    [InlineData("DsaApkInstaller", "AppNameText")]
    [InlineData("DsaApkInstaller", "SectionHeader")]
    public void Fr17_a_title_style_never_uses_a_weight_korean_text_renders_bold(string app, string key)
    {
        var doc = AppXamlOf(app);
        var style = doc.Descendants(Wpf + "Style").Single(s => (string?)s.Attribute(X + "Key") == key);
        var weight = Setter(style, "FontWeight");
        Assert.True(weight == "Normal",
            $"{app} style {key} has FontWeight {weight}; Malgun Gothic would draw Korean text in Bold");
    }

    [Fact]
    public void Fr17_no_settings_xaml_sets_a_weight_korean_text_renders_bold()
    {
        var root = Path.GetDirectoryName(Path.GetDirectoryName(XamlPath("DsaSettings", "App.xaml")))!;
        string[] heavy = { "\"Medium\"", "\"SemiBold\"", "\"DemiBold\"", "\"Bold\"", "\"ExtraBold\"", "\"Black\"" };
        foreach (var file in Directory.GetFiles(Path.Combine(root, "DsaSettings"), "*.xaml", SearchOption.AllDirectories)
            .Concat(Directory.GetFiles(Path.Combine(root, "DsaApkInstaller"), "*.xaml", SearchOption.AllDirectories)))
        {
            var text = File.ReadAllText(file);
            Assert.False(heavy.Any(h => text.Contains("FontWeight=" + h) || text.Contains("Property=\"FontWeight\" Value=" + h)),
                file + " sets a heavy FontWeight; Malgun Gothic would draw Korean text in Bold");
            Assert.False(text.Contains("{StaticResource BodyStrong}"),
                file + " uses the library's BodyStrong style, which is SemiBold; use SectionHeader");
        }
    }

    private static string XamlPath(string app, string file)
    {
        string[] relative = { Path.Combine(app, file),
            Path.Combine("host", "platform", "windows", "settings", app, file) };
        string? found = null;
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null && found is null; dir = dir.Parent)
        {
            found = relative.Select(r => Path.Combine(dir.FullName, r)).FirstOrDefault(File.Exists);
        }
        Assert.True(found is not null, file + " was not found above " + AppContext.BaseDirectory);
        return found!;
    }

    private static XDocument AppXamlOf(string app) => XDocument.Load(XamlPath(app, "App.xaml"));

    private static XElement KeyedStyle(string key) => AppXaml().Descendants(Wpf + "Style")
        .Single(s => (string?)s.Attribute(X + "Key") == key);

    private static XElement TypedStyle(string type) => AppXaml().Descendants(Wpf + "Style")
        .Single(s => (string?)s.Attribute("TargetType") == type);

    private static string? Setter(XElement style, string property) => style.Elements(Wpf + "Setter")
        .FirstOrDefault(s => (string?)s.Attribute("Property") == property)?.Attribute("Value")?.Value;

    private static string? Resource(string key) => AppXaml().Root!.Descendants()
        .FirstOrDefault(e => (string?)e.Attribute(X + "Key") == key && !e.HasElements)?.Value;

    [Theory]
    // The WinUI type ramp: Title, Subtitle, Body Strong, Body, Caption. The strong ones get
    // their Latin weight from a Semibold face, not from FontWeight.
    [InlineData("PageTitle", "28", "36", "Normal")]
    [InlineData("SubtitleText", "20", "28", "Normal")]
    [InlineData("SectionHeader", "14", "20", "Normal")]
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

    [Theory]
    [InlineData("ui:Button", "DefaultUiButtonStyle")]
    [InlineData("ui:CardControl", "DefaultUiCardControlStyle")]
    [InlineData("ui:CardAction", "DefaultUiCardActionStyle")]
    [InlineData("ui:ToggleSwitch", "DefaultUiToggleSwitchStyle")]
    [InlineData("ui:NavigationViewItem", "NavigationViewItemDefaultStyle")]
    [InlineData("ToolTip", "DefaultToolTipStyle")]
    [InlineData("ContextMenu", "UiContextMenu")]
    public void Fr17_a_restyled_control_is_based_on_the_library_style(string type, string libraryStyle)
    {
        Assert.Equal("{StaticResource " + libraryStyle + "}", (string?)TypedStyle(type).Attribute("BasedOn"));
    }

    [Fact]
    public void Fr17_a_button_keeps_the_library_template()
    {
        Assert.DoesNotContain(TypedStyle("ui:Button").Elements(Wpf + "Setter"),
            s => (string?)s.Attribute("Property") == "Template");
    }

    [Fact]
    public void Fr17_no_style_is_based_on_its_own_type_key()
    {
        // A style so based loses the library's setters, and a button then draws as a Win32 button.
        var offenders = AppXaml().Descendants(Wpf + "Style")
            .Where(s => ((string?)s.Attribute("BasedOn"))?.Contains("x:Type") == true)
            .Select(s => (string?)s.Attribute("TargetType"))
            .ToList();
        Assert.True(offenders.Count == 0,
            "App.xaml bases these styles on a type key, not on the library's named style: "
            + string.Join(", ", offenders));
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
    public void Fr17_every_dynamic_resource_in_the_app_styles_exists()
    {
        // WPF says nothing about a key it cannot find: the property keeps its default, and a
        // state that only that key paints never shows. The library's compiled XAML stores each
        // key as a string with its length before it.
        var path = typeof(CardStyleTests).Assembly.GetCustomAttributes<AssemblyMetadataAttribute>()
            .Single(a => a.Key == "WpfUiAssembly").Value!;
        Assert.True(File.Exists(path), "WPF UI 3.0.5 was not found at " + path);
        var library = Encoding.Latin1.GetString(File.ReadAllBytes(path));
        bool InLibrary(string key) => library.Contains((char)key.Length + key, StringComparison.Ordinal);

        Assert.True(InLibrary("ToggleSwitchFillOn"), "the search does not find a key the library has");
        Assert.False(InLibrary("ToggleSwitchFill"), "the search matches part of a longer key");

        var app = AppXaml();
        var own = app.Descendants().Select(e => (string?)e.Attribute(X + "Key")).OfType<string>().ToHashSet();
        var missing = app.Descendants().SelectMany(e => e.Attributes())
            .SelectMany(a => Regex.Matches(a.Value, @"\{DynamicResource (\w+)\}").Select(m => m.Groups[1].Value))
            .Distinct()
            .Where(key => !own.Contains(key) && !InLibrary(key))
            .ToList();
        Assert.True(missing.Count == 0,
            "App.xaml uses resource keys that neither it nor WPF UI 3.0.5 defines: " + string.Join(", ", missing));
    }

    [Fact]
    public void Fr17_an_on_switch_takes_the_accent_fill_from_the_library_style()
    {
        // Based on anything else, Background is the system toggle button's #DDDDDD until the
        // pointer-over trigger paints it, which is an on switch that is accent only under the mouse.
        var style = TypedStyle("ui:ToggleSwitch");
        Assert.Equal("{StaticResource DefaultUiToggleSwitchStyle}", (string?)style.Attribute("BasedOn"));
        Assert.Null(Setter(style, "Background"));
        var track = style.Descendants(Wpf + "Rectangle").Single(e => (string?)e.Attribute(X + "Name") == "ActiveToggleRectangle");
        Assert.Equal("{TemplateBinding Background}", (string?)track.Attribute("Fill"));
    }

    [Theory]
    [InlineData("ToggleEllipse", "False", "17", "3,0,0,0")]
    [InlineData("ActiveToggleEllipse", "True", "17", "0,0,3,0")]
    public void Fr17_a_toggle_knob_grows_as_in_windows_11(string knob, string isChecked, string pressedWidth, string pressedMargin)
    {
        var style = TypedStyle("ui:ToggleSwitch");
        var rest = style.Descendants(Wpf + "Ellipse").Single(e => (string?)e.Attribute(X + "Name") == knob);
        Assert.Equal("12", (string?)rest.Attribute("Width"));
        Assert.Equal("12", (string?)rest.Attribute("Height"));
        Assert.Equal("-10", (string?)rest.Descendants(Wpf + "TranslateTransform").Single().Attribute("X"));

        string? When(string condition, string property) => style.Descendants(Wpf + "MultiTrigger")
            .Where(t => t.Descendants(Wpf + "Condition").Any(c => (string?)c.Attribute("Property") == condition)
                && t.Descendants(Wpf + "Condition").Any(c => (string?)c.Attribute("Property") == "IsChecked" && (string?)c.Attribute("Value") == isChecked))
            .SelectMany(t => t.Elements(Wpf + "Setter"))
            .FirstOrDefault(s => (string?)s.Attribute("TargetName") == knob && (string?)s.Attribute("Property") == property)
            ?.Attribute("Value")?.Value;

        Assert.Equal("14", When("IsMouseOver", "Width"));
        Assert.Equal("14", When("IsMouseOver", "Height"));
        Assert.Equal(pressedWidth, When("IsPressed", "Width"));
        Assert.Equal("14", When("IsPressed", "Height"));
        Assert.Equal(pressedMargin, When("IsPressed", "Margin"));
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

    private static readonly XNamespace Ui = "http://schemas.lepo.co/wpfui/2022/xaml";

    private static XElement Named(XDocument doc, string name) =>
        doc.Descendants().Single(e => (string?)e.Attribute(X + "Name") == name);

    private static XElement PaneTemplate(XDocument doc) => doc.Descendants(Wpf + "ControlTemplate")
        .Single(e => (string?)e.Attribute(X + "Key") == "PaneTemplate");

    private static XElement PaneItemTemplate(XDocument doc) => doc.Descendants(Wpf + "ControlTemplate")
        .Single(e => (string?)e.Attribute(X + "Key") == "PaneItemTemplate");

    private static double[] Thickness(string? value) => value!.Split(',').Select(double.Parse).ToArray();

    [Fact]
    public void Fr17_the_navigation_pane_has_no_back_button()
    {
        var window = Xaml("MainWindow.xaml");
        var nav = window.Descendants(Ui + "NavigationView").Single();
        Assert.Equal("Collapsed", (string?)nav.Attribute("IsBackButtonVisible"));
        Assert.Equal("{StaticResource PaneTemplate}", (string?)nav.Attribute("Template"));
        Assert.Equal("{StaticResource PaneItemTemplate}", (string?)nav.Attribute("ItemTemplate"));
        Assert.DoesNotContain(window.Descendants(), e => (string?)e.Attribute(X + "Name") == "PART_BackButton");
    }

    [Fact]
    public void Fr17_the_title_bar_lines_up_with_the_navigation_pane()
    {
        var window = Xaml("MainWindow.xaml");
        var bar = window.Descendants(Ui + "TitleBar").Single();
        Assert.Equal("48", (string?)bar.Attribute("Height"));
        Assert.Null(bar.Attribute("Padding")); // the library's 16 px from the window's edge

        const double titleBarPadding = 16;
        const double titleBarIconGap = 12; // the library's, after the icon
        var icon = window.Descendants(Ui + "ImageIcon").Single();
        var iconWidth = double.Parse(((string?)icon.Attribute("Width"))!);
        var iconRight = Thickness((string?)icon.Attribute("Margin"))[2];

        var paneLeft = Thickness((string?)Named(window, "PaneGrid").Attribute("Margin"))[0];
        var iconColumn = double.Parse(((string?)PaneItemTemplate(window).Descendants(Wpf + "ColumnDefinition").First().Attribute("Width"))!);
        var labelMargin = Thickness((string?)Named(window, "ElementContentPresenter").Attribute("Margin"))[0];
        var toggle = Named(window, "PART_ToggleButton");

        // The icon column: title bar, hamburger and items share one centre line, which stays
        // where it is when the pane closes to the icon column's width.
        Assert.Equal(24, titleBarPadding + iconWidth / 2);
        Assert.Equal(24, paneLeft + iconColumn / 2);
        Assert.Equal(24, paneLeft + double.Parse(((string?)toggle.Attribute("Width"))!) / 2);
        Assert.Contains("To=\"40\"", PaneTemplate(window).ToString());

        // The label column: the title starts where the labels do, twice the icon's width from
        // the icon's left edge.
        var titleLeft = titleBarPadding + iconWidth + iconRight + titleBarIconGap;
        Assert.Equal(48, titleLeft);
        Assert.Equal(48, paneLeft + iconColumn + labelMargin);
        Assert.Equal(2 * iconWidth, titleLeft - titleBarPadding);
    }

    [Fact]
    public void Fr17_the_pane_and_the_page_start_right_under_the_title_bar()
    {
        var window = Xaml("MainWindow.xaml");
        // A closed install bar must not leave its margins between the title bar and the pane.
        Assert.Equal("{Binding IsOpen, Converter={StaticResource Visible}}",
            (string?)Named(window, "InstallBar").Attribute("Visibility"));
        var card = Named(window, "ContentCard");
        Assert.Equal("8,0,0,0", (string?)card.Attribute("CornerRadius"));
        Assert.Equal("{TemplateBinding FrameMargin}", (string?)card.Attribute("Margin"));
        Assert.Equal("0", (string?)Named(window, "PART_ToggleButton").Attribute("Grid.Row"));
    }

    [Fact]
    public void Fr17_navigation_items_have_task_managers_spacing_and_selection()
    {
        var style = TypedStyle("ui:NavigationViewItem");
        Assert.Equal("36", Setter(style, "MinHeight"));
        Assert.Equal("0,2,0,2", Setter(style, "Margin"));

        var window = Xaml("MainWindow.xaml");
        Assert.Equal("36", (string?)Named(window, "MainBorder").Attribute("Height"));
        Assert.Equal("{DynamicResource ControlCornerRadius}", (string?)Named(window, "MainBorder").Attribute("CornerRadius"));
        var pill = Named(window, "ActiveRectangle");
        Assert.Equal("3", (string?)pill.Attribute("Width"));
        Assert.Equal("16", (string?)pill.Attribute("Height"));
        Assert.Equal("{DynamicResource NavigationViewSelectionIndicatorForeground}", (string?)pill.Attribute("Fill"));
        var triggers = PaneItemTemplate(window).ToString();
        Assert.Contains("NavigationViewItemBackgroundSelected", triggers);
        Assert.Contains("NavigationViewItemBackgroundPointerOver", triggers);
    }

    [Fact]
    public void Fr17_the_window_keeps_its_pane_width_and_backdrop()
    {
        var window = Xaml("MainWindow.xaml");
        Assert.Equal("236", (string?)window.Descendants(Ui + "NavigationView").Single().Attribute("OpenPaneLength"));
        Assert.Equal("Mica", (string?)window.Root!.Attribute("WindowBackdropType"));
    }
}
