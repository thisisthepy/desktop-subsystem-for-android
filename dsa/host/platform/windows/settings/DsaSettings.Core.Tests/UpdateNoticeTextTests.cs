using System.Globalization;
using System.Text.RegularExpressions;
using System.Xml.Linq;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>The General and Updates pages show the service's update notice in the app's language.
/// The service writes it in English for `dsa status`; the Korean General page showed "Android was
/// updated to 17.0.0-16373615-12" and "DSA was updated to 0.2.1" under a Korean title.</summary>
public class UpdateNoticeTextTests
{
    /// <summary>Gives each sentence as its key and its values, so a test sees which sentence a
    /// line became and what was filled in.</summary>
    private sealed class SentenceText : ITextSource
    {
        public string Get(string key) => UpdateNoticeText.AllKeys.TryGetValue(key, out var count)
            ? key + "(" + string.Join("|", Enumerable.Range(0, count).Select(i => "{" + i + "}")) + ")"
            : key;
    }

    public UpdateNoticeTextTests()
    {
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
    }

    private static async Task<GeneralViewModel> General(string? notice)
    {
        var json = "{\"state\":\"Active\",\"last_reason\":null,\"update_notice\":"
            + (notice is null ? "null" : System.Text.Json.JsonSerializer.Serialize(notice)) + "}";
        var vm = new GeneralViewModel(new DsaService(new FakeCli().Answer("status --json", json)), new SentenceText());
        await vm.RefreshAsync();
        return vm;
    }

    [Fact]
    public async Task Fr17_the_update_notice_is_shown_in_the_app_language()
    {
        // The two lines the Korean General page showed in English.
        var vm = await General("Android was updated to 17.0.0-16373615-12\nDSA was updated to 0.2.1");
        Assert.True(vm.HasUpdateNotice);
        Assert.Equal("UpdateNotice_AndroidUpdated(17.0.0-16373615-12)\nUpdateNotice_DsaUpdated(0.2.1)", vm.UpdateNotice);
    }

    [Fact]
    public async Task Fr17_no_notice_shows_no_bar()
    {
        var vm = await General(null);
        Assert.False(vm.HasUpdateNotice);
        Assert.Equal("", vm.UpdateNotice);
    }

    [Fact]
    public async Task Fr17_a_notice_line_the_app_does_not_know_is_shown_as_the_service_wrote_it()
    {
        var vm = await General("something new happened to Android\nDSA was updated to 0.2.1");
        Assert.Equal("something new happened to Android\nUpdateNotice_DsaUpdated(0.2.1)", vm.UpdateNotice);
    }

    // Each sentence the service writes, as it writes it (dsa/host/core/src/update).
    [Theory]
    [InlineData("Android was updated to 17.0.0-1", "UpdateNotice_AndroidUpdated(17.0.0-1)")]
    [InlineData("Android 17.0.0-1 is installed", "UpdateNotice_AndroidInstalled(17.0.0-1)")]
    [InlineData("Android 16 is chosen; the service downloads it now and it is used the next time Android starts",
        "UpdateNotice_AndroidChosen(16)")]
    [InlineData("Android 17.0.0-2 did not start (the agent did not connect (after 300 s)), so Android 17.0.0-1 is in use again. "
        + "Android 17.0.0-2 is not tried again until a newer build of it or a new version of DSA is available, or until "
        + "Android 17 is chosen again in the settings app (dsa android use 17)",
        "UpdateNotice_AndroidRolledBack(17.0.0-2|the agent did not connect (after 300 s)|17.0.0-1)")]
    [InlineData("Android 17.0.0-2 did not start (it crashed), so Android 17.0.0-1 is in use again, but it has not finished "
        + "starting yet (still booting). Start Android again (dsa start) to boot Android 17.0.0-1. Android 17.0.0-2 is not "
        + "tried again until a newer build of it or a new version of DSA is available, or until Android 17 is chosen again "
        + "in the settings app (dsa android use 17)",
        "UpdateNotice_AndroidRolledBackNotReady(17.0.0-2|it crashed|17.0.0-1|still booting)")]
    [InlineData("Android 17.0.0-2 did not start (it crashed); no other image is installed. The service downloads another "
        + "image if there is one. To try this one again, choose Android 17 in the settings app (dsa android use 17); the "
        + "next version of DSA also tries it again",
        "UpdateNotice_AndroidGaveUp(17.0.0-2|it crashed)")]
    [InlineData("No Android image is installed, because the images available for this computer failed to start on it. "
        + "Android 17.0.0-2 did not start: the guest did not start: it crashed. Android 16.0.0-1 did not start. "
        + "The service does not download them again by itself. To try again, choose a version under Updates in the "
        + "settings app (or press Check now there), or run `dsa android use 17`. If it fails again, collect the logs "
        + "with `dsa logs collect -o dsa-logs.zip` and attach the file to your report.",
        "UpdateNotice_AndroidFailedHere(Android 17.0.0-2 did not start: the guest did not start: it crashed. "
        + "Android 16.0.0-1 did not start.|17)")]
    [InlineData("No Android image is installed, and no image to install is available yet (supported: Android 16, 17); "
        + "the service keeps checking",
        "UpdateNotice_AndroidNoImageAvailable(16, 17)")]
    [InlineData("Android 16.0.0-1 did not start on this computer before and is tried again: the service downloads it now, "
        + "and it boots the next time Android starts (going back if it fails again)",
        "UpdateNotice_AndroidRetried(16.0.0-1)")]
    [InlineData("Android 17.0.0-2 is kept on trial and is not to blame: the host went to sleep",
        "UpdateNotice_AndroidOnTrial(17.0.0-2|the host went to sleep)")]
    [InlineData("DSA was updated to 0.2.1", "UpdateNotice_DsaUpdated(0.2.1)")]
    [InlineData("DSA 0.2.2 is downloaded and installs when Android is not running (`dsa update` installs it now)",
        "UpdateNotice_DsaDownloaded(0.2.2)")]
    [InlineData("installing DSA 0.2.2; the service restarts when it is done", "UpdateNotice_DsaInstalling(0.2.2)")]
    [InlineData("updating DSA to 0.2.2 failed (setup exited with 1603); DSA 0.2.1 is still installed",
        "UpdateNotice_DsaFailed(0.2.2|setup exited with 1603|0.2.1)")]
    public void Fr17_every_notice_the_service_writes_has_a_sentence(string line, string expected)
    {
        Assert.Equal(expected, UpdateNoticeText.Localize(new SentenceText(), line));
    }

    private static Dictionary<string, string> Strings(string file)
    {
        string[] relative = { Path.Combine("DsaSettings", file),
            Path.Combine("host", "platform", "windows", "settings", "DsaSettings", file) };
        string? found = null;
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null && found is null; dir = dir.Parent)
        {
            found = relative.Select(r => Path.Combine(dir.FullName, r)).FirstOrDefault(File.Exists);
        }
        Assert.True(found is not null, file + " was not found above " + AppContext.BaseDirectory);
        return XDocument.Load(found!).Descendants("data")
            .ToDictionary(d => (string)d.Attribute("name")!, d => d.Element("value")!.Value);
    }

    [Theory]
    [MemberData(nameof(StringTables.Files), MemberType = typeof(StringTables))]
    public void Fr17_every_notice_sentence_exists_in_every_language_with_its_values(string file)
    {
        var strings = Strings(file);
        foreach (var (key, count) in UpdateNoticeText.AllKeys)
        {
            Assert.True(strings.ContainsKey(key), $"{file} has no {key}");
            var used = Regex.Matches(strings[key], @"\{(\d+)\}").Select(m => int.Parse(m.Groups[1].Value)).Distinct().Order();
            Assert.Equal(Enumerable.Range(0, count), used);
        }
    }

    [Fact]
    public void Fr17_korean_notices_are_korean()
    {
        var strings = Strings("Strings.ko.resx");
        foreach (var key in UpdateNoticeText.AllKeys.Keys)
        {
            Assert.Matches(@"[가-힣]", strings[key]);
        }
    }
}
