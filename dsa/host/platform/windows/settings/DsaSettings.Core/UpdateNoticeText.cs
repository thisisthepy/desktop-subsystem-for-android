using System.Globalization;
using System.Text.RegularExpressions;

namespace Dsa.Settings.Core;

/// <summary>Puts the service's update notice into the user's language. The service writes it in
/// English for `dsa status`, one line per notice (one for the Android image, one for DSA), so
/// each line is matched to the kind of news it gives, and that kind's sentence is filled in with
/// the versions the line names. A line of a kind this app does not know is shown as the service
/// wrote it rather than hidden. A reason inside a line (why an image did not start, why setup
/// failed) is the service's own words and stays as it is.</summary>
public static partial class UpdateNoticeText
{
    public const string AndroidUpdated = "UpdateNotice_AndroidUpdated";
    public const string AndroidInstalled = "UpdateNotice_AndroidInstalled";
    public const string AndroidChosen = "UpdateNotice_AndroidChosen";
    public const string AndroidRolledBack = "UpdateNotice_AndroidRolledBack";
    public const string AndroidRolledBackNotReady = "UpdateNotice_AndroidRolledBackNotReady";
    public const string AndroidGaveUp = "UpdateNotice_AndroidGaveUp";
    public const string AndroidOnTrial = "UpdateNotice_AndroidOnTrial";
    public const string AndroidFailedHere = "UpdateNotice_AndroidFailedHere";
    public const string AndroidNoImageAvailable = "UpdateNotice_AndroidNoImageAvailable";
    public const string AndroidRetried = "UpdateNotice_AndroidRetried";
    public const string DsaUpdated = "UpdateNotice_DsaUpdated";
    public const string DsaDownloaded = "UpdateNotice_DsaDownloaded";
    public const string DsaInstalling = "UpdateNotice_DsaInstalling";
    public const string DsaFailed = "UpdateNotice_DsaFailed";

    /// <summary>Every sentence a notice line can map to, with how many values it takes.</summary>
    public static readonly IReadOnlyDictionary<string, int> AllKeys = new Dictionary<string, int>
    {
        [AndroidUpdated] = 1,
        [AndroidInstalled] = 1,
        [AndroidChosen] = 1,
        [AndroidRolledBack] = 3,
        [AndroidRolledBackNotReady] = 4,
        [AndroidGaveUp] = 2,
        [AndroidOnTrial] = 2,
        [AndroidFailedHere] = 2,
        [AndroidNoImageAvailable] = 1,
        [AndroidRetried] = 1,
        [DsaUpdated] = 1,
        [DsaDownloaded] = 1,
        [DsaInstalling] = 1,
        [DsaFailed] = 3,
    };

    // The service's sentences, in the order they are tried: a longer sentence before one that
    // starts the same way.
    private static readonly (Regex Line, string Key)[] Lines =
    {
        (new(@"^Android was updated to (\S+)$"), AndroidUpdated),
        (new(@"^Android (\S+) is installed$"), AndroidInstalled),
        (new(@"^Android (\S+) is chosen; "), AndroidChosen),
        (new(@"^Android (.+) did not start on this computer before and is tried again: "), AndroidRetried),
        // Which images failed and why stays in the service's words, as every reason does.
        (new(@"^No Android image is installed, because the images available for this computer failed to start on it\. (.*) The service does not download them again by itself\. .*`dsa android use ([^`\s]+)`"),
            AndroidFailedHere),
        (new(@"^No Android image is installed, and no image to install is available yet \(supported: Android ([^)]*)\); the service keeps checking$"),
            AndroidNoImageAvailable),
        (new(@"^Android (\S+) did not start \((.*)\), so Android (\S+) is in use again, but it has not finished starting yet \((.*)\)\. "),
            AndroidRolledBackNotReady),
        (new(@"^Android (\S+) did not start \((.*)\), so Android (\S+) is in use again\. "), AndroidRolledBack),
        (new(@"^Android (\S+) did not start \((.*)\); no other image is installed\. "), AndroidGaveUp),
        (new(@"^Android (\S+) is kept on trial and is not to blame: (.*)$"), AndroidOnTrial),
        (new(@"^DSA was updated to (\S+)$"), DsaUpdated),
        (new(@"^DSA (\S+) is downloaded and installs when Android is not running"), DsaDownloaded),
        (new(@"^installing DSA (\S+); the service restarts when it is done$"), DsaInstalling),
        (new(@"^updating DSA to (\S+) failed \((.*)\); DSA (\S+) is still installed$"), DsaFailed),
    };

    /// <summary>The kind of news <paramref name="line"/> gives and the values it names, or null
    /// for a line this app does not know.</summary>
    public static (string Key, string[] Values)? Parse(string line)
    {
        foreach (var (pattern, key) in Lines)
        {
            var match = pattern.Match(line);
            if (match.Success)
            {
                return (key, match.Groups.Cast<Group>().Skip(1).Select(g => g.Value).ToArray());
            }
        }
        return null;
    }

    /// <summary><paramref name="notice"/> in the user's language, line by line.</summary>
    public static string Localize(ITextSource text, string notice) => string.Join("\n",
        notice.Split('\n').Select(line => line.TrimEnd('\r')).Where(line => line.Length > 0).Select(line =>
            Parse(line) is { } parsed
                ? string.Format(CultureInfo.CurrentCulture, text.Get(parsed.Key), parsed.Values)
                : line));
}
