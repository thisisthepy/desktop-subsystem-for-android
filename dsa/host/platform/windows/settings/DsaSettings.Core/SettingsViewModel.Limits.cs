using System.Globalization;

namespace Dsa.Settings.Core;

/// <summary>One choice of memory limit: what the list shows and the size in MiB (0 is
/// automatic).</summary>
public sealed record MemoryLimitChoice(string Label, int Mb);

/// <summary>The memory limit and the diagnostic trace switches.</summary>
public sealed partial class SettingsViewModel
{
    /// <summary>The smallest limit the service takes, in MiB. Android does not start in less.</summary>
    public const int SmallestMemoryLimitMb = 2048;

    private static readonly int[] OfferedLimitsMb = { 2048, 3072, 4096, 6144, 8192, 12288, 16384 };

    /// <summary>The most memory the user's Android may grow to, in MiB; 0 is automatic.</summary>
    public int MemoryLimitMb => _settings?.MemoryLimitMb ?? 0;

    /// <summary>The limit as the page names it: automatic, or its size.</summary>
    public string MemoryLimitText => MemoryLimitMb == 0 ? _text.Get("MemoryLimit_Auto") : LimitSize(MemoryLimitMb);

    private static string LimitSize(int mb) => mb % 1024 == 0
        ? string.Format(CultureInfo.CurrentCulture, "{0} GB", mb / 1024)
        : string.Format(CultureInfo.CurrentCulture, "{0} MB", mb);

    /// <summary>Automatic, then the sizes on offer, then a size set elsewhere (with
    /// <c>dsa settings set memory-limit</c>) that is none of them.</summary>
    public IReadOnlyList<MemoryLimitChoice> MemoryLimitChoices
    {
        get
        {
            var choices = new List<MemoryLimitChoice> { new(_text.Get("MemoryLimit_Auto"), 0) };
            choices.AddRange(OfferedLimitsMb.Select(mb => new MemoryLimitChoice(LimitSize(mb), mb)));
            if (MemoryLimitMb != 0 && !OfferedLimitsMb.Contains(MemoryLimitMb))
            {
                choices.Add(new MemoryLimitChoice(LimitSize(MemoryLimitMb), MemoryLimitMb));
                choices.Sort((a, b) => a.Mb == 0 ? -1 : b.Mb == 0 ? 1 : a.Mb.CompareTo(b.Mb));
            }
            return choices;
        }
    }

    public int SelectedMemoryLimitIndex
    {
        get
        {
            var choices = MemoryLimitChoices;
            for (var i = 0; i < choices.Count; i++)
            {
                if (choices[i].Mb == MemoryLimitMb)
                {
                    return i;
                }
            }
            return 0;
        }
    }

    /// <summary>Sets the limit, in MiB (0 for automatic). A size below the smallest the
    /// service takes changes nothing.</summary>
    public Task SetMemoryLimitAsync(int mb)
    {
        if (mb is < 0 or > 0 and < SmallestMemoryLimitMb)
        {
            ChangedAll();
            return Task.CompletedTask;
        }
        return mb == MemoryLimitMb && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetMemoryLimitAsync(mb));
    }

    /// <summary>Whether every DSA process writes the input trace to its log.</summary>
    public bool InputTrace => _settings?.InputTrace ?? false;

    /// <summary>Whether every DSA process writes each stage of each resize to its log.</summary>
    public bool ResizeTrace => _settings?.ResizeTrace ?? false;

    /// <summary>The traces are the computer's settings, so only an administrator changes
    /// them.</summary>
    public bool CanChangeTraces => Loaded && !_busy && _administrator;

    /// <summary>Whether the traces can be changed only by running dsa as an administrator.</summary>
    public bool TracesNeedAdministrator => Loaded && !_busy && !_administrator;

    public Task SetInputTraceAsync(bool on) =>
        on == InputTrace && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetInputTraceAsync(on));

    public Task SetResizeTraceAsync(bool on) =>
        on == ResizeTrace && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetResizeTraceAsync(on));
}
