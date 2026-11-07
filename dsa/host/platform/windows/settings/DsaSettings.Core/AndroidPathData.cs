using System.Globalization;
using System.Text;

namespace Dsa.Settings.Core;

/// <summary>
/// A vector drawable's path data in the form WPF's path markup reads. Android writes numbers
/// the way SVG allows, with no separator where none is needed (<c>M1.5.5l-2-3</c>), and WPF's
/// parser needs one between every number. Each command and number comes out separated by a
/// space, in the invariant culture.
/// </summary>
public static class AndroidPathData
{
    private const string Commands = "MmLlHhVvCcSsQqTtAaZz";

    public static string ToWpf(string data)
    {
        var tokens = new List<string>();
        var i = 0;
        while (i < data.Length)
        {
            var c = data[i];
            if (Commands.IndexOf(c) >= 0)
            {
                tokens.Add(c.ToString());
                i++;
            }
            else if (c is '-' or '+' or '.' || char.IsAsciiDigit(c))
            {
                var start = i;
                if (c is '-' or '+')
                {
                    i++;
                }
                var dot = false;
                while (i < data.Length && (char.IsAsciiDigit(data[i]) || (data[i] == '.' && !dot)))
                {
                    dot |= data[i] == '.';
                    i++;
                }
                if (i < data.Length && data[i] is 'e' or 'E')
                {
                    var exponent = i + 1;
                    if (exponent < data.Length && data[exponent] is '-' or '+')
                    {
                        exponent++;
                    }
                    if (exponent < data.Length && char.IsAsciiDigit(data[exponent]))
                    {
                        i = exponent;
                        while (i < data.Length && char.IsAsciiDigit(data[i]))
                        {
                            i++;
                        }
                    }
                }
                var text = data[start..i];
                tokens.Add(double.TryParse(text, NumberStyles.Float, CultureInfo.InvariantCulture, out var value)
                    ? value.ToString("R", CultureInfo.InvariantCulture)
                    : "0");
            }
            else
            {
                // Spaces, commas and anything else separate.
                i++;
            }
        }
        var output = new StringBuilder();
        foreach (var token in tokens)
        {
            if (output.Length > 0)
            {
                output.Append(' ');
            }
            output.Append(token);
        }
        return output.ToString();
    }
}
