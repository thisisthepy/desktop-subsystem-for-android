using System.Runtime;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Threading;
using Dsa.Settings.Core;

namespace Dsa.Settings;

/// <summary>Gives memory back to Windows when the window is minimised or hidden, and compacts
/// the heap after a page is left (<see cref="MemoryTrim"/>). The work waits until the window
/// is idle, so a page change is drawn first.</summary>
internal static class WorkingSetTrim
{
    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetProcessWorkingSetSize(IntPtr process, IntPtr minimum, IntPtr maximum);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GetCurrentProcess();

    public static void Attach(Window window)
    {
        window.StateChanged += (_, _) =>
        {
            if (window.WindowState == WindowState.Minimized)
            {
                Later(window, WindowChange.Minimized);
            }
        };
        window.IsVisibleChanged += (_, e) =>
        {
            if (e.NewValue is false)
            {
                Later(window, WindowChange.Hidden);
            }
        };
    }

    public static void PageLeft(Window window) => Later(window, WindowChange.PageLeft);

    // One trim waits at a time: a burst of window changes folds into it rather than queueing
    // a full garbage collection for each change.
    private static readonly TrimQueue Queue = new();

    private static void Later(Window window, WindowChange change)
    {
        if (Queue.Request(change))
        {
            window.Dispatcher.BeginInvoke(DispatcherPriority.ApplicationIdle, () => Apply(Queue.Take()));
        }
    }

    private static void Apply(MemoryTrim trim)
    {
        if (trim.CompactHeap)
        {
            GCSettings.LargeObjectHeapCompactionMode = GCLargeObjectHeapCompactionMode.CompactOnce;
            GC.Collect(GC.MaxGeneration, GCCollectionMode.Forced, blocking: true, compacting: true);
            GC.WaitForPendingFinalizers();
            GC.Collect(GC.MaxGeneration, GCCollectionMode.Forced, blocking: true, compacting: true);
        }
        if (trim.EmptyWorkingSet)
        {
            // -1 and -1 take every page out of the working set; Windows keeps them on the
            // standby list, so coming back costs soft faults, not reads from disk.
            SetProcessWorkingSetSize(GetCurrentProcess(), -1, -1);
        }
    }
}
