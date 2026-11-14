using System.Text.RegularExpressions;
using System.Xml.Linq;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>Every text the settings app and the APK Installer show is short enough to read at a
/// glance. A paragraph (the text between two line breaks) is at most 140 columns, where a
/// Hangul or other wide character takes two, because it is about twice as wide on screen; that
/// keeps an English and a Korean paragraph to about the same width. Longer texts are split into
/// lines, a list, or paragraphs separated by a blank line. The Widevine confirmation was one
/// 400-character paragraph, and card descriptions ran to 450.</summary>
public class TextLengthTests
{
    private const int MaxColumns = 140;

    /// <summary>The bodies of the app's confirmation and message dialogs.</summary>
    public static readonly string[] DialogBodies =
    {
        "ArmGoogle_Notice", "Widevine_Notice", "Backup_DeleteQuestion", "Backup_RestoreWarning",
        "ResetData_Warning", "AppsRunAsArmEraseBody", "AppsUninstallConfirmBody", "IdentityResetConfirmBody",
        "RestartWindowManagerBody", "AndroidRestartConfirm.Body", "Android_DownWarning", "AndroidUpConfirm.Body",
        "UpdateNowBody", "FolderMissingBody",
    };

    private static Dictionary<string, string> Strings(string file)
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
        return XDocument.Load(found!).Descendants("data")
            .ToDictionary(d => (string)d.Attribute("name")!, d => d.Element("value")?.Value ?? "");
    }

    /// <summary>How wide a line is in columns: two for a wide character, one for any other.</summary>
    public static int Columns(string line) => line.Sum(c => c switch
    {
        >= 'ᄀ' and <= 'ᅟ' => 2,
        >= '⺀' and <= '꓏' => 2,
        >= '가' and <= '힣' => 2,
        >= '豈' and <= '﫿' => 2,
        >= '＀' and <= '｠' => 2,
        _ => 1,
    });

    private static string[] Lines(string text) => text.Replace("\r\n", "\n").Split('\n');

    /// <summary>What each line is: blank, a list item, or text. English and Korean must agree.</summary>
    private static string Shape(string text) => string.Join("|", Lines(text).Select(line => line.Trim() switch
    {
        "" => "blank",
        var l when l.StartsWith("• ", StringComparison.Ordinal) => "bullet",
        var l when Regex.IsMatch(l, @"^\d+\. ") => "step",
        _ => "text",
    }));

    [Fact]
    public void Fr17_columns_count_hangul_twice()
    {
        Assert.Equal(4, Columns("ab"+"한"));
        Assert.Equal(5, Columns("Hello"));
    }

    [Theory]
    [InlineData("Strings.resx")]
    [InlineData("Strings.ko.resx")]
    public void Fr17_no_text_has_a_paragraph_wider_than_140_columns(string file)
    {
        var offenders = Strings(file)
            .SelectMany(s => Lines(s.Value).Where(line => Columns(line) > MaxColumns)
                .Select(line => $"{s.Key} ({Columns(line)} columns): {line}"))
            .ToList();
        Assert.True(offenders.Count == 0,
            $"{file} has paragraphs over {MaxColumns} columns; shorten them or split them into lines: "
            + string.Join(" || ", offenders));
    }

    [Fact]
    public void Fr17_english_and_korean_break_each_text_into_the_same_lines()
    {
        var english = Strings("Strings.resx");
        var korean = Strings("Strings.ko.resx");
        Assert.Equal(english.Keys.Order(), korean.Keys.Order());
        var offenders = english.Keys
            .Where(key => Shape(english[key]) != Shape(korean[key]))
            .Select(key => $"{key}: en {Shape(english[key])}, ko {Shape(korean[key])}")
            .ToList();
        Assert.True(offenders.Count == 0,
            "these texts are split differently in English and Korean, so they do not say the same things in the same order: "
            + string.Join("; ", offenders));
    }

    [Theory]
    [InlineData("Strings.resx")]
    [InlineData("Strings.ko.resx")]
    public void Fr17_a_dialog_body_longer_than_a_line_is_split_into_paragraphs(string file)
    {
        var strings = Strings(file);
        foreach (var key in DialogBodies)
        {
            Assert.True(strings.ContainsKey(key), $"{file} has no {key}");
            var body = strings[key].Replace("\r\n", "\n");
            if (Columns(body) > 100)
            {
                Assert.True(body.Contains("\n\n"),
                    $"{file}: the dialog body {key} is {Columns(body)} columns in one block; put the lead first and each fact after it on its own line");
            }
        }
    }

    [Theory]
    [InlineData("Strings.resx", "2 GB")]
    [InlineData("Strings.ko.resx", "2GB")]
    public void Fr37_the_widevine_confirmation_states_source_terms_size_and_the_sd_limit(string file, string size)
    {
        var notice = Strings(file)["Widevine_Notice"];
        Assert.Contains("{0}", notice);
        Assert.Contains("{1}", notice);
        Assert.Contains(size, notice);
        Assert.Contains("L3", notice);
        Assert.Contains("SD", notice);
        // A lead paragraph, then the facts as a list.
        Assert.Contains("\n\n• ", notice.Replace("\r\n", "\n"));
    }
}
