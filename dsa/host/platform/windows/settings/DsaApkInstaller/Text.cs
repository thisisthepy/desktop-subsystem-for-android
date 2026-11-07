using System.Globalization;
using System.Resources;
using Dsa.Settings.Core;

namespace Dsa.ApkInstaller;

/// <summary>The window's texts, in English in every language, from the settings app's
/// English string table (Strings.resx), which this app embeds as its own.</summary>
internal sealed class ResourceText : ITextSource
{
    private static readonly ResourceManager Manager =
        new("Dsa.ApkInstaller.Strings", typeof(ResourceText).Assembly);

    public string Get(string key)
    {
        try
        {
            return Manager.GetString(key, CultureInfo.InvariantCulture) ?? key;
        }
        catch (MissingManifestResourceException)
        {
            return key;
        }
    }
}
