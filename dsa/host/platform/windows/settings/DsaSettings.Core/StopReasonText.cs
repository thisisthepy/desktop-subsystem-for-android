using System.Globalization;
using System.Text.RegularExpressions;

namespace Dsa.Settings.Core;

/// <summary>Turns the service's account of why Android stopped into the key of a short sentence
/// for the user. The account is written for logs and can hold a whole HCS document, so it is
/// shown only when the user asks for the details.</summary>
public static partial class StopReasonText
{
    public const string Requested = "LastReason_Requested";
    public const string Updating = "LastReason_Updating";
    public const string Virtualization = "LastReason_Virtualization";
    public const string InvalidConfiguration = "LastReason_InvalidConfiguration";
    public const string OutOfMemory = "LastReason_OutOfMemory";
    public const string Disk = "LastReason_Disk";
    public const string TimedOut = "LastReason_TimedOut";
    public const string Lost = "LastReason_Lost";
    public const string General = "LastReason_General";

    /// <summary>Every sentence a reason can map to.</summary>
    public static readonly IReadOnlyList<string> AllKeys = new[]
    {
        Requested, Updating, Virtualization, InvalidConfiguration, OutOfMemory, Disk, TimedOut, Lost, General,
    };

    // Single phrases only. The HCS document names "Memory" and "Disk" as settings, so those
    // words alone say nothing about what failed.
    private static readonly (string Phrase, string Key)[] Phrases =
    {
        ("host compute system is not available", Virtualization),
        ("virtual machine platform", Virtualization),
        ("out of memory", OutOfMemory),
        ("not enough memory", OutOfMemory),
        ("cannot allocate memory", OutOfMemory),
        ("no space left on device", Disk),
        ("not enough space", Disk),
        ("the agent did not connect", TimedOut),
        ("the control channel was lost", Lost),
        ("android was stopped so that it does not keep failing", Lost),
    };

    [GeneratedRegex(@"\b0x([0-9a-fA-F]{8})\b")]
    private static partial Regex Code();

    /// <summary>The sentence's key for <paramref name="reason"/>. The error code counts only on
    /// the first line: the lines below it are the guest's console, whose hex numbers are
    /// addresses.</summary>
    public static string KeyFor(string reason)
    {
        if (reason == "stopped on request")
        {
            return Requested;
        }
        if (reason.StartsWith("DSA ", StringComparison.Ordinal) && reason.Contains("is being installed", StringComparison.Ordinal))
        {
            return Updating;
        }
        var firstLine = reason.Split('\n', 2)[0];
        foreach (Match match in Code().Matches(firstLine))
        {
            var key = KeyForCode(uint.Parse(match.Groups[1].Value, NumberStyles.HexNumber, CultureInfo.InvariantCulture));
            if (key is not null)
            {
                return key;
            }
        }
        foreach (var (phrase, key) in Phrases)
        {
            if (reason.Contains(phrase, StringComparison.OrdinalIgnoreCase))
            {
                return key;
            }
        }
        return General;
    }

    private static string? KeyForCode(uint code) => code switch
    {
        0x80370102 or 0xC0351000 => Virtualization, // Hyper-V not installed, no hypervisor present
        0x80070057 or 0x8037010D => InvalidConfiguration, // invalid argument, invalid JSON
        0x8007000E or 0x80070008 or 0x800705AA or 0x800705AF => OutOfMemory,
        0x80070070 or 0x80070027 or 0x80070017 or 0x8007045D => Disk, // disk full, CRC, I/O error
        _ => null,
    };
}
