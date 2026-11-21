using System.Collections.ObjectModel;

namespace Dsa.Settings.Core;

/// <summary>Runs one piece of work at a time: a call that comes while the work runs joins it
/// instead of starting it again. A page polls on a timer, and a poll that takes longer than the
/// timer's interval would otherwise start another on every tick, each one waiting for the
/// window's thread, until the window spends all its time catching up.</summary>
public sealed class SingleFlight
{
    private Task? _running;

    /// <summary>Whether the work is running now.</summary>
    public bool Running => _running is { IsCompleted: false };

    public Task RunAsync(Func<Task> work)
    {
        if (_running is { IsCompleted: false } running)
        {
            return running;
        }
        _running = work();
        return _running;
    }
}

/// <summary>A row of a list on a page whose words change while it stays on screen.</summary>
public sealed class LiveRow : ObservableObject
{
    private string _label;
    private string _detail;
    private string _size;

    public LiveRow(string label, string detail, string size)
    {
        _label = label;
        _detail = detail;
        _size = size;
    }

    public string Label => _label;

    public string Detail => _detail;

    public string Size => _size;

    internal void Set(MemoryRow row)
    {
        if (_label != row.Label)
        {
            _label = row.Label;
            Changed(nameof(Label));
        }
        if (_detail != row.Detail)
        {
            _detail = row.Detail;
            Changed(nameof(Detail));
        }
        if (_size != row.Size)
        {
            _size = row.Size;
            Changed(nameof(Size));
        }
    }
}

/// <summary>A list of rows that stays the same list across refreshes: a row that is still
/// there keeps its object and only its changed words are told, and rows come and go at the
/// end. A list handed out anew on each refresh made the window throw away and rebuild every
/// row's controls every few seconds and, with a screen reader or any other UI Automation client
/// attached, announce every rebuilt control again: a flood of events that kept the window's
/// thread busy and held up the other windows on the desktop.</summary>
public sealed class LiveRows : ReadOnlyObservableCollection<LiveRow>
{
    private readonly ObservableCollection<LiveRow> _rows;

    public LiveRows() : this(new ObservableCollection<LiveRow>())
    {
    }

    private LiveRows(ObservableCollection<LiveRow> rows) : base(rows)
    {
        _rows = rows;
    }

    public void Update(IReadOnlyList<MemoryRow> rows)
    {
        for (var i = 0; i < rows.Count; i++)
        {
            if (i < _rows.Count)
            {
                _rows[i].Set(rows[i]);
            }
            else
            {
                _rows.Add(new LiveRow(rows[i].Label, rows[i].Detail, rows[i].Size));
            }
        }
        while (_rows.Count > rows.Count)
        {
            _rows.RemoveAt(_rows.Count - 1);
        }
    }
}

/// <summary>Lets at most one trim of the settings app's memory wait at a time. The window asks
/// for one when it is minimised, hidden or leaves a page; asks that come while one waits are
/// folded into it, and the run does the most any of them asked for. Each ask used to queue a
/// run of its own, so a burst of window changes queued a burst of full garbage collections.</summary>
public sealed class TrimQueue
{
    private MemoryTrim? _pending;

    /// <summary>Adds <paramref name="change"/>'s trim. True when the caller should queue a run;
    /// false when a run already waits, which now covers this one too, or there is nothing to do.</summary>
    public bool Request(WindowChange change)
    {
        var trim = MemoryTrim.After(change);
        if (!trim.CompactHeap && !trim.EmptyWorkingSet)
        {
            return false;
        }
        if (_pending is { } waiting)
        {
            _pending = new MemoryTrim(waiting.CompactHeap || trim.CompactHeap,
                waiting.EmptyWorkingSet || trim.EmptyWorkingSet);
            return false;
        }
        _pending = trim;
        return true;
    }

    /// <summary>The trim to do now, which empties the queue.</summary>
    public MemoryTrim Take()
    {
        var trim = _pending ?? new MemoryTrim(false, false);
        _pending = null;
        return trim;
    }
}

/// <summary>The space at a page's sides: 56 px in a wide window and 36 px in a narrow one, as
/// Windows' Settings app has it; 1008 px is WinUI's wide breakpoint.</summary>
public static class PageMargins
{
    public const double WideWindowWidth = 1008;

    public static double Side(double windowWidth) => windowWidth >= WideWindowWidth ? 56 : 36;
}
