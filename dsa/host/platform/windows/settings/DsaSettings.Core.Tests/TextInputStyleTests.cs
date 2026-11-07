using Xunit;
using System.Xml.Linq;

namespace Dsa.Settings.Core.Tests;

/// <summary>The library's text box draws a 1 px dark focus rectangle around itself, and its
/// selectable mode keeps that rectangle while dropping the padding, so a read-only value looked
/// like a cramped outlined box. Editable boxes take the library's look through App.xaml, and a
/// read-only value is plain selectable text.</summary>
public class TextInputStyleTests
{
    private static readonly XNamespace Wpf = "http://schemas.microsoft.com/winfx/2006/xaml/presentation";

    private static readonly XNamespace Ui = "http://schemas.lepo.co/wpfui/2022/xaml";

    private static readonly XNamespace X = "http://schemas.microsoft.com/winfx/2006/xaml";

    private static string SettingsFolder()
    {
        // The tests run from dsa/target/settings/..., outside the source tree, so each folder
        // above is tried both as the settings folder and as the repository's dsa folder.
        string[] relative = { "DsaSettings", Path.Combine("host", "platform", "windows", "settings", "DsaSettings") };
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null; dir = dir.Parent)
        {
            var found = relative.Select(r => Path.Combine(dir.FullName, r))
                .FirstOrDefault(p => File.Exists(Path.Combine(p, "App.xaml")));
            if (found is not null)
            {
                return Path.GetDirectoryName(found)!;
            }
        }
        throw new DirectoryNotFoundException("no settings folder above " + AppContext.BaseDirectory);
    }

    private static IEnumerable<(string File, XDocument Xaml)> AllXaml()
    {
        var root = SettingsFolder();
        return new[] { "DsaSettings", "DsaApkInstaller" }
            .SelectMany(app => Directory.EnumerateFiles(Path.Combine(root, app), "*.xaml", SearchOption.AllDirectories))
            .Where(f => !f.Split(Path.DirectorySeparatorChar).Any(p => p is "bin" or "obj"))
            .Select(f => (Path.GetFileName(f), XDocument.Load(f)));
    }

    private static XDocument File_(string relative) =>
        XDocument.Load(Path.Combine(SettingsFolder(), "DsaSettings", relative));

    private static string? Setter(XElement style, string property) => style.Elements(Wpf + "Setter")
        .FirstOrDefault(s => (string?)s.Attribute("Property") == property)?.Attribute("Value")?.Value;

    private static XElement Style(string key) => File_("App.xaml").Descendants(Wpf + "Style")
        .Single(s => (string?)s.Attribute(X + "Key") == key);

    private static XElement Implicit(string type) => File_("App.xaml").Descendants(Wpf + "Style")
        .Single(s => (string?)s.Attribute("TargetType") == type && s.Attribute(X + "Key") is null);

    private static IEnumerable<XElement> TextBoxes(XDocument xaml) => xaml.Descendants()
        .Where(e => e.Name == Wpf + "TextBox" || e.Name == Ui + "TextBox");

    [Theory]
    [InlineData("ui:TextBox", "DefaultUiTextBoxStyle")]
    [InlineData("TextBox", "DefaultTextBoxStyle")]
    public void Fr17_a_text_box_keeps_the_library_look_without_its_focus_rectangle(string type, string libraryStyle)
    {
        var style = Implicit(type);
        Assert.Equal("{StaticResource " + libraryStyle + "}", (string?)style.Attribute("BasedOn"));
        Assert.Equal("{x:Null}", Setter(style, "FocusVisualStyle"));
        Assert.Equal("Center", Setter(style, "VerticalContentAlignment"));
        Assert.DoesNotContain(style.Elements(Wpf + "Setter"), s => (string?)s.Attribute("Property") == "Template");
    }

    [Fact]
    public void Fr17_a_read_only_value_is_never_the_library_text_box()
    {
        var offenders = AllXaml()
            .SelectMany(x => x.Xaml.Descendants(Ui + "TextBox")
                .Where(e => e.Attribute("IsTextSelectionEnabled") is not null || e.Attribute("IsReadOnly") is not null)
                .Select(e => x.File + ": " + (string?)e.Attribute("Text")))
            .ToList();
        Assert.True(offenders.Count == 0,
            "These read-only values use the library's text box, which draws a focus rectangle around cramped text: "
            + string.Join("; ", offenders));
    }

    [Fact]
    public void Fr17_an_editable_box_does_not_override_the_library_metrics()
    {
        string[] overrides = { "MinHeight", "Height", "Padding", "BorderThickness", "BorderBrush", "Background", "Foreground", "FontFamily", "FontSize", "Style", "Template" };
        var offenders = AllXaml()
            .SelectMany(x => TextBoxes(x.Xaml)
                .Where(e => e.Parent?.Name != Wpf + "ControlTemplate" && e.Attribute("Style") is null)
                .SelectMany(e => overrides.Where(o => e.Attribute(o) is not null).Select(o => x.File + ": " + o)))
            .ToList();
        Assert.True(offenders.Count == 0, "These boxes override the look they take from the library: " + string.Join("; ", offenders));
    }

    [Fact]
    public void Fr17_every_plain_text_box_with_a_style_is_selectable_text()
    {
        var styles = new[] { "{StaticResource SelectableText}", "{StaticResource SelectableCode}" };
        var offenders = AllXaml()
            .SelectMany(x => x.Xaml.Descendants(Wpf + "TextBox")
                .Where(e => !styles.Contains((string?)e.Attribute("Style")))
                .Select(e => x.File + ": " + ((string?)e.Attribute(X + "Name") ?? (string?)e.Attribute("Text"))))
            .ToList();
        Assert.True(offenders.Count == 0, "Use ui:TextBox to edit and the selectable styles to read: " + string.Join("; ", offenders));
    }

    [Fact]
    public void Fr17_selectable_text_has_no_frame_tab_stop_or_focus_mark()
    {
        var style = Style("SelectableText");
        Assert.Equal("True", Setter(style, "IsReadOnly"));
        Assert.Equal("False", Setter(style, "IsTabStop"));
        Assert.Equal("{x:Null}", Setter(style, "FocusVisualStyle"));
        var template = style.Descendants(Wpf + "ControlTemplate").Single();
        Assert.Empty(template.Descendants(Wpf + "Border"));
        Assert.Equal("{StaticResource SelectableText}", (string?)Style("SelectableCode").Attribute("BasedOn"));
    }

    [Fact]
    public void Fr17_the_adb_command_has_its_own_filled_row_with_a_copy_button()
    {
        var block = Style("CodeBlock");
        Assert.Equal("{DynamicResource SubtleFillColorSecondaryBrush}", Setter(block, "Background"));
        Assert.Equal("4", Setter(block, "CornerRadius"));
        Assert.Equal("0,8,0,0", Setter(block, "Margin"));

        var page = File_(Path.Combine("Pages", "AdvancedPage.xaml"));
        var command = page.Descendants(Wpf + "TextBox").Single(e => (string?)e.Attribute(X + "Name") == "AdbCommand");
        var border = command.Ancestors(Wpf + "Border").First();
        Assert.Equal("{StaticResource CodeBlock}", (string?)border.Attribute("Style"));
        Assert.Contains(border.Descendants(Ui + "Button"), b => (string?)b.Attribute("Click") == "OnCopyAdbCommand");
    }

    [Fact]
    public void Fr17_the_adb_port_is_a_library_box_with_a_body_text_label()
    {
        var page = File_(Path.Combine("Pages", "AdvancedPage.xaml"));
        var box = page.Descendants(Ui + "TextBox").Single(e => (string?)e.Attribute(X + "Name") == "AdbPortBox");
        var label = box.Parent!.Elements(Wpf + "TextBlock").Single();
        Assert.Equal("{StaticResource CardTitle}", (string?)label.Attribute("Style"));
        Assert.Equal("Center", (string?)label.Attribute("VerticalAlignment"));
    }
}
