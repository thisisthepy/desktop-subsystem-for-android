using System.Globalization;
using System.Resources;
using System.Windows.Markup;
using Dsa.Settings.Core;

namespace Dsa.Settings;

/// <summary>The texts in the user's language, from Strings.resx (English) and
/// Strings.&lt;language&gt;.resx. .NET picks the language from the user's display
/// language.</summary>
internal sealed class ResourceText : ITextSource
{
    private static readonly ResourceManager Manager =
        new("Dsa.Settings.Strings", typeof(ResourceText).Assembly);

    public string Get(string key)
    {
        try
        {
            return Manager.GetString(key, CultureInfo.CurrentUICulture) ?? key;
        }
        catch (MissingManifestResourceException)
        {
            return key;
        }
    }
}

/// <summary>A text in the user's language, in XAML: <c>{l:Text NavGeneral}</c>.</summary>
[MarkupExtensionReturnType(typeof(string))]
public sealed class TextExtension : MarkupExtension
{
    public TextExtension()
    {
    }

    public TextExtension(string key)
    {
        Key = key;
    }

    [ConstructorArgument("key")]
    public string Key { get; set; } = "";

    public override object ProvideValue(IServiceProvider serviceProvider) => App.Text.Get(Key);
}
