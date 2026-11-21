namespace Dsa.Settings.Core;

/// <summary>Whether a page's polling timers run: only while the page is loaded and its window
/// is shown and not minimised. A page nobody can see has nothing to keep current, and each
/// poll starts a <c>dsa</c> process and allocates its answer.</summary>
public static class PagePolling
{
    public static bool ShouldPoll(bool loaded, bool windowVisible, bool windowMinimized) =>
        loaded && windowVisible && !windowMinimized;
}

/// <summary>The size an app icon is decoded at: the size it is shown at in device pixels,
/// never more than the icon itself. Android hands out 256 px icons; a list of them decoded at
/// that size holds 256 KiB for each, where a 32 px icon on a 150% monitor needs 9 KiB.</summary>
public static class IconDecodeSize
{
    /// <param name="shownDip">The size the icon is shown at, in device independent pixels.</param>
    /// <param name="scale">The monitor's scale (1.5 at 144 dpi); less than 1 counts as 1.</param>
    /// <param name="sourcePixels">The icon's own width in pixels, or 0 when unknown.</param>
    public static int Pixels(double shownDip, double scale, int sourcePixels)
    {
        var wanted = (int)Math.Ceiling(shownDip * Math.Max(scale, 1.0));
        wanted = Math.Max(wanted, 1);
        return sourcePixels > 0 ? Math.Min(wanted, sourcePixels) : wanted;
    }
}

/// <summary>What the settings app gives back to Windows after something in its window changes.
/// A page that is left leaves its controls and their data to the garbage collector; the window
/// stays on screen, so only the heap is compacted. A minimised or hidden window draws nothing
/// until it comes back, so its working set goes back to Windows as well; the pages it touches
/// again come back on use.</summary>
public enum WindowChange
{
    PageLeft,
    Minimized,
    Hidden,
    Restored,
    /// <summary>A page has polled <see cref="PollCompaction.Every"/> times since the last
    /// compaction.</summary>
    Polled,
}

public readonly record struct MemoryTrim(bool CompactHeap, bool EmptyWorkingSet)
{
    public static MemoryTrim After(WindowChange change) => change switch
    {
        WindowChange.PageLeft or WindowChange.Polled => new(CompactHeap: true, EmptyWorkingSet: false),
        WindowChange.Minimized or WindowChange.Hidden => new(CompactHeap: true, EmptyWorkingSet: true),
        _ => new(CompactHeap: false, EmptyWorkingSet: false),
    };
}

/// <summary>When a page that stays open compacts the heap. Each poll allocates its answer and
/// the rows built from it; what outlives a young collection reaches the oldest generation,
/// which a window that stays on one page never collects. A dump of a window left on one page
/// for an hour held 10 MB of dead objects in a 23 MB heap. A compaction after every
/// <see cref="Every"/> polls (a minute at the Memory page's five seconds) takes a few
/// milliseconds on a heap this small.</summary>
public sealed class PollCompaction
{
    public const int Every = 12;

    private int _polls;

    /// <summary>Counts a finished poll. True when the heap is due a compaction now.</summary>
    public bool Polled()
    {
        _polls++;
        if (_polls < Every)
        {
            return false;
        }
        _polls = 0;
        return true;
    }
}
