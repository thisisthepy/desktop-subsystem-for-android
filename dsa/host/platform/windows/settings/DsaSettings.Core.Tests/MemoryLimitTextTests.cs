using System.Text.RegularExpressions;
using System.Xml.Linq;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>Automatic has no fixed limit on this host: the VM starts small and grows while it
/// runs up to the PC's memory less what Windows keeps. The Memory page has to say so, because
/// a size named there reads as a ceiling the user would then raise by hand.</summary>
public class MemoryLimitTextTests
{
    private static string Description(string file)
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
            .Single(d => (string?)d.Attribute("name") == "MemoryLimitCard.Description")
            .Element("value")!.Value;
    }

    [Theory]
    [InlineData("Strings.resx")]
    [InlineData("Strings.ko.resx")]
    public void Fr17_automatic_memory_limit_names_no_fixed_size(string file)
    {
        var text = Description(file);
        Assert.False(Regex.IsMatch(text, @"\d\s*(GB|GiB)", RegexOptions.IgnoreCase),
            $"{file} names a size, which reads as a limit on Automatic: {text}");
        Assert.True(text.Contains("Windows"),
            $"{file} should say that Automatic stops at the PC's memory less what Windows keeps: {text}");
    }
}
