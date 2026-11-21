using System.Text.RegularExpressions;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>Every text the settings app and the APK Installer show is short enough to read at a
/// glance, in every interface language. A paragraph (the text between two line breaks) is at
/// most 140 columns, where a wide character (Hangul, kana, Han and the full-width forms) takes
/// two, because it is about twice as wide on screen; that keeps a paragraph to about the same
/// width in every language. Longer texts are split into lines, a list, or paragraphs separated
/// by a blank line. The Widevine confirmation was one 400-character paragraph, and card
/// descriptions ran to 450.
///
/// Each translation says the same things as English in the same order: the same keys, the same
/// format holes, and the same lines, list items and blank lines.</summary>
public class TextLengthTests
{
    private const int MaxColumns = 140;

    /// <summary>U+2014, which no DSA text uses; written by its number, so this file has none.</summary>
    public const char EmDash = (char)0x2014;

    /// <summary>The bodies of the app's confirmation and message dialogs.</summary>
    public static readonly string[] DialogBodies =
    {
        "ArmGoogle_Notice", "Widevine_Notice", "Backup_DeleteQuestion", "Backup_RestoreWarning",
        "ResetData_Warning", "AppsRunAsArmEraseBody", "AppsUninstallConfirmBody", "IdentityResetConfirmBody",
        "RestartWindowManagerBody", "AndroidRestartConfirm.Body", "Android_DownWarning", "AndroidUpConfirm.Body",
        "UpdateNowBody", "FolderMissingBody", "RestartConfirm.Body",
    };

    private static Dictionary<string, string> Strings(string file) => StringTables.Load(file);

    /// <summary>How wide a line is in columns: two for a wide character, one for any other.</summary>
    public static int Columns(string line) => line.Sum(c => c switch
    {
        >= 'ᄀ' and <= 'ᅟ' => 2,
        // CJK radicals, symbols and punctuation, Hiragana, Katakana, Bopomofo, Hangul
        // compatibility jamo, CJK unified ideographs, Yi.
        >= '⺀' and <= '꓏' => 2,
        >= '가' and <= '힣' => 2,
        >= '豈' and <= '﫿' => 2,
        // Vertical forms, and the CJK compatibility and small form variants.
        >= (char)0xFE10 and <= (char)0xFE19 => 2,
        >= (char)0xFE30 and <= (char)0xFE6F => 2,
        // Full-width forms and signs.
        >= (char)0xFF00 and <= (char)0xFF60 => 2,
        >= (char)0xFFE0 and <= (char)0xFFE6 => 2,
        _ => 1,
    });

    private static string[] Lines(string text) => text.Replace("\r\n", "\n").Split('\n');

    /// <summary>What each line is: blank, a list item, or text. Every language must agree.</summary>
    private static string Shape(string text) => string.Join("|", Lines(text).Select(line => line.Trim() switch
    {
        "" => "blank",
        var l when l.StartsWith("• ", StringComparison.Ordinal) => "bullet",
        var l when Regex.IsMatch(l, @"^\d+\. ") => "step",
        _ => "text",
    }));

    [Fact]
    public void Fr17_columns_count_wide_characters_twice()
    {
        Assert.Equal(4, Columns("ab" + "한"));
        Assert.Equal(5, Columns("Hello"));
        // Hiragana, Katakana, Han, the ideographic full stop and comma, full-width forms.
        Assert.Equal(2, Columns("あ"));
        Assert.Equal(2, Columns("カ"));
        Assert.Equal(4, Columns("设置"));
        Assert.Equal(4, Columns("設定"));
        Assert.Equal(4, Columns("。、"));
        Assert.Equal(6, Columns(new string(new[] { (char)0xFF08, (char)0xFF21, (char)0xFF09 })));
        Assert.Equal(2, Columns(((char)0xFFE5).ToString()));
        // Latin with accents and Cyrillic are one column each.
        Assert.Equal(13, Columns("Einstellungen"));
        Assert.Equal(5, Columns("Größe"));
        Assert.Equal(9, Columns("Параметры"));
    }

    [Theory]
    [MemberData(nameof(StringTables.Translations), MemberType = typeof(StringTables))]
    public void Fr17_every_interface_language_has_its_string_table(string file)
    {
        Assert.True(StringTables.Path(file) is not null,
            $"{file} is missing beside Strings.resx, so that language falls back to English");
    }

    [Theory]
    [MemberData(nameof(StringTables.Translations), MemberType = typeof(StringTables))]
    public void Fr17_every_table_has_exactly_the_english_keys(string file)
    {
        var english = Strings("Strings.resx");
        var translation = Strings(file);
        var missing = english.Keys.Where(k => !translation.ContainsKey(k)).Order().ToList();
        var extra = translation.Keys.Where(k => !english.ContainsKey(k)).Order().ToList();
        Assert.True(missing.Count == 0 && extra.Count == 0,
            $"{file} lacks {string.Join(", ", missing)}; it has keys English does not: {string.Join(", ", extra)}");
    }

    [Theory]
    [MemberData(nameof(StringTables.Files), MemberType = typeof(StringTables))]
    public void Fr17_no_text_is_empty(string file)
    {
        var empty = Strings(file).Where(s => string.IsNullOrWhiteSpace(s.Value)).Select(s => s.Key).ToList();
        Assert.True(empty.Count == 0, $"{file} has empty texts: {string.Join(", ", empty)}");
    }

    [Theory]
    [MemberData(nameof(StringTables.Translations), MemberType = typeof(StringTables))]
    public void Fr17_every_text_has_the_format_holes_of_english(string file)
    {
        var english = Strings("Strings.resx");
        var translation = Strings(file);
        var offenders = english.Keys.Where(translation.ContainsKey)
            .Where(key => !StringTables.Holes(english[key]).SequenceEqual(StringTables.Holes(translation[key])))
            .Select(key => $"{key}: en {string.Join(",", StringTables.Holes(english[key]))}, " +
                $"{file} {string.Join(",", StringTables.Holes(translation[key]))}")
            .ToList();
        Assert.True(offenders.Count == 0,
            $"these texts in {file} take other values than in English, so a value would be lost or the text would not format: "
            + string.Join("; ", offenders));
    }

    [Theory]
    [MemberData(nameof(StringTables.Files), MemberType = typeof(StringTables))]
    public void Fr17_no_text_has_an_em_dash(string file)
    {
        var offenders = Strings(file).Where(s => s.Value.Contains(EmDash)).Select(s => s.Key).ToList();
        Assert.True(offenders.Count == 0, $"{file} has an em dash in: {string.Join(", ", offenders)}");
    }

    [Theory]
    [MemberData(nameof(StringTables.Files), MemberType = typeof(StringTables))]
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

    [Theory]
    [MemberData(nameof(StringTables.Translations), MemberType = typeof(StringTables))]
    public void Fr17_every_language_breaks_each_text_into_the_same_lines_as_english(string file)
    {
        var english = Strings("Strings.resx");
        var translation = Strings(file);
        var offenders = english.Keys.Where(translation.ContainsKey)
            .Where(key => Shape(english[key]) != Shape(translation[key]))
            .Select(key => $"{key}: en {Shape(english[key])}, {file} {Shape(translation[key])}")
            .ToList();
        Assert.True(offenders.Count == 0,
            $"these texts are split differently in English and in {file}, so they do not say the same things in the same order: "
            + string.Join("; ", offenders));
    }

    [Theory]
    [MemberData(nameof(StringTables.Files), MemberType = typeof(StringTables))]
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
    [MemberData(nameof(StringTables.Files), MemberType = typeof(StringTables))]
    public void Fr37_the_widevine_confirmation_states_source_terms_size_and_the_sd_limit(string file)
    {
        var notice = Strings(file)["Widevine_Notice"];
        Assert.Contains("{0}", notice);
        Assert.Contains("{1}", notice);
        // The download's size, in GB in every language.
        Assert.Matches(@"\d\s?GB", notice);
        Assert.Contains("L3", notice);
        Assert.Contains("SD", notice);
        // A lead paragraph, then the facts as a list.
        Assert.Contains("\n\n• ", notice.Replace("\r\n", "\n"));
    }

    [Theory]
    [InlineData("Strings.resx", "2 GB")]
    [InlineData("Strings.ko.resx", "2GB")]
    public void Fr37_the_widevine_confirmation_writes_its_size_as_the_language_does(string file, string size)
    {
        Assert.Contains(size, Strings(file)["Widevine_Notice"]);
    }
}
