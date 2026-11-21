using System.Collections.Specialized;
using System.ComponentModel;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>The settings app hung the desktop with its Memory page open: every refresh handed
/// the page new row lists, the window rebuilt every row's controls, and with a UI Automation
/// client attached (Task Manager, a screen reader) it raised an automation event for each
/// rebuilt control through the system's window event hooks, which every other window waits
/// on. Refreshes that came while one ran piled up behind it. These tests hold the fixes: the
/// rows stay the same objects, refreshes do not overlap, and window changes queue at most one
/// trim.</summary>
public class Nfr1SettingsAppHangTests
{
    private static string Report(ulong vmKb, int processes)
    {
        var top = string.Join(",", Enumerable.Range(0, processes)
            .Select(i => $"{{\"name\":\"p{i}\",\"pid\":{100 + i},\"pss_kb\":{1024 * (i + 1)}}}"));
        return "{\"host\":[{\"role\":\"vm\",\"pid\":10,\"working_set_kb\":" + vmKb + ",\"private_kb\":900000}," +
            "{\"role\":\"window manager\",\"pid\":20,\"working_set_kb\":40960,\"private_kb\":30000}]," +
            "\"host_total_kb\":" + (vmKb + 40960) + ",\"guest\":{\"mem_total_kb\":2097152," +
            "\"mem_available_kb\":1048576,\"top_processes\":[" + top + "]}}";
    }

    [Fact]
    public async Task Nfr1_memory_page_keeps_its_rows_across_refreshes()
    {
        var cli = new FakeCli().Answer("memory --json", Report(921600, 3));
        var vm = new MemoryViewModel(new DsaService(cli), new KeyText());
        await vm.RefreshAsync();
        var hostList = vm.HostRows;
        var processList = vm.GuestProcessRows;
        var vmRow = vm.HostRows[0];
        var guestRows = vm.GuestRows.ToList();
        var reset = 0;
        var replaced = 0;
        NotifyCollectionChangedEventHandler count = (_, e) =>
        {
            if (e.Action == NotifyCollectionChangedAction.Reset)
            {
                reset++;
            }
            if (e.Action == NotifyCollectionChangedAction.Replace)
            {
                replaced++;
            }
        };
        ((INotifyCollectionChanged)vm.HostRows).CollectionChanged += count;
        ((INotifyCollectionChanged)vm.GuestRows).CollectionChanged += count;
        var told = new List<string?>();
        vmRow.PropertyChanged += (_, e) => told.Add(e.PropertyName);
        var listsTold = new List<string?>();
        vm.PropertyChanged += (_, e) => listsTold.Add(e.PropertyName);

        // The VM grows by 100 MiB: one size changes, nothing else.
        cli.Answer("memory --json", Report(1024000, 3));
        await vm.RefreshAsync();

        Assert.Same(hostList, vm.HostRows);
        Assert.Same(processList, vm.GuestProcessRows);
        Assert.Same(vmRow, vm.HostRows[0]);
        Assert.Equal(guestRows, vm.GuestRows.ToList());
        Assert.Equal(0, reset);
        Assert.Equal(0, replaced);
        Assert.Equal(new[] { nameof(LiveRow.Size) }, told);
        Assert.Equal("1000.0 MB", vmRow.Size);

        // A process more and then fewer: rows come and go at the end, the rest stay.
        var first = vm.GuestProcessRows[0];
        cli.Answer("memory --json", Report(1024000, 5));
        await vm.RefreshAsync();
        Assert.Equal(5, vm.GuestProcessRows.Count);
        Assert.Same(first, vm.GuestProcessRows[0]);
        cli.Answer("memory --json", Report(1024000, 1));
        await vm.RefreshAsync();
        Assert.Single(vm.GuestProcessRows);
        Assert.Same(first, vm.GuestProcessRows[0]);
    }

    /// <summary>Answers <c>memory --json</c> only when the test lets it, counting the calls.</summary>
    private sealed class HeldCli : IDsaCli
    {
        public TaskCompletionSource<CliResult> Answer { get; private set; } = new();
        public int Calls { get; private set; }

        public Task<CliResult> RunAsync(IReadOnlyList<string> args, CancellationToken cancel = default)
        {
            Calls++;
            return Answer.Task;
        }

        public void Release(string json)
        {
            var answer = Answer;
            Answer = new TaskCompletionSource<CliResult>();
            answer.SetResult(new CliResult(0, json, ""));
        }
    }

    [Fact]
    public async Task Nfr1_a_memory_refresh_asked_for_while_one_runs_joins_it()
    {
        var cli = new HeldCli();
        var vm = new MemoryViewModel(new DsaService(cli), new KeyText());
        // The page's first showing, its timer and its Refresh button, all before the answer.
        var refreshes = Enumerable.Range(0, 24).Select(_ => vm.RefreshAsync()).ToList();
        Assert.Equal(1, cli.Calls);
        cli.Release(Report(921600, 2));
        await Task.WhenAll(refreshes);
        Assert.Equal(2, vm.HostRows.Count);
        // Once it is done, the next refresh asks again.
        var next = vm.RefreshAsync();
        Assert.Equal(2, cli.Calls);
        cli.Release(Report(921600, 2));
        await next;
    }

    [Fact]
    public async Task Nfr1_single_flight_runs_one_poll_at_a_time()
    {
        var flight = new SingleFlight();
        var runs = 0;
        var held = new TaskCompletionSource();
        Task Poll()
        {
            runs++;
            return held.Task;
        }
        var first = flight.RunAsync(Poll);
        Assert.True(flight.Running);
        Assert.Same(first, flight.RunAsync(Poll));
        Assert.Equal(1, runs);
        held.SetResult();
        await first;
        Assert.False(flight.Running);
        held = new TaskCompletionSource();
        _ = flight.RunAsync(Poll);
        Assert.Equal(2, runs);
        held.SetResult();
    }

    [Fact]
    public void Nfr1_window_changes_queue_at_most_one_memory_trim()
    {
        var queue = new TrimQueue();
        Assert.True(queue.Request(WindowChange.PageLeft));
        // A burst of changes while that one waits folds into it.
        Assert.False(queue.Request(WindowChange.Minimized));
        Assert.False(queue.Request(WindowChange.Hidden));
        Assert.False(queue.Request(WindowChange.PageLeft));
        Assert.Equal(new MemoryTrim(true, true), queue.Take());
        // A change that asks for nothing queues nothing.
        Assert.False(queue.Request(WindowChange.Restored));
        Assert.Equal(new MemoryTrim(false, false), queue.Take());
        Assert.True(queue.Request(WindowChange.PageLeft));
        Assert.Equal(new MemoryTrim(true, false), queue.Take());
    }

    [Fact]
    public void Fr17_page_margins_change_only_at_the_wide_breakpoint()
    {
        Assert.Equal(36, PageMargins.Side(640));
        Assert.Equal(36, PageMargins.Side(1007.9));
        Assert.Equal(56, PageMargins.Side(1008));
        Assert.Equal(56, PageMargins.Side(1600));
    }

    [Fact]
    public async Task Nfr1_the_memory_page_lets_go_of_its_report_when_left()
    {
        var cli = new FakeCli().Answer("memory --json", Report(921600, 3));
        var vm = new MemoryViewModel(new DsaService(cli), new KeyText());
        await vm.RefreshAsync();
        var hostList = vm.HostRows;
        vm.Release();
        Assert.Null(vm.Report);
        Assert.Empty(vm.HostRows);
        Assert.Empty(vm.GuestProcessRows);
        await vm.RefreshAsync();
        Assert.Same(hostList, vm.HostRows);
        Assert.Equal(2, vm.HostRows.Count);
    }

    [Fact]
    public void Nfr1_pages_with_lists_let_go_of_them_when_left_and_the_app_ends_with_its_window()
    {
        foreach (var page in new[] { "AppsPage.xaml.cs", "BackgroundPage.xaml.cs", "MemoryPage.xaml.cs", "LogsPage.xaml.cs" })
        {
            var code = File.ReadAllText(SettingsSources.File("DsaSettings", "Pages", page));
            Assert.Matches(@"Unloaded \+= \(_, _\) =>\s*(\{[^}]*)?ViewModel\.Release\(\)", code);
        }
        var app = File.ReadAllText(SettingsSources.File("DsaSettings", "App.xaml"));
        Assert.Contains("ShutdownMode=\"OnLastWindowClose\"", app);
    }

    [Fact]
    public void Nfr1_page_timers_skip_a_tick_while_their_poll_runs()
    {
        var pages = Path.Combine(SettingsSources.Dir("DsaSettings", "Pages"));
        foreach (var file in Directory.GetFiles(pages, "*.xaml.cs"))
        {
            foreach (var line in File.ReadAllLines(file).Where(l => l.Contains(".Tick +=")))
            {
                Assert.Contains("PollingTimers.Once(", line);
            }
        }
        var window = File.ReadAllText(SettingsSources.File("DsaSettings", "MainWindow.xaml.cs"));
        Assert.Contains("if (side == _pageSide)", window);
        var trim = File.ReadAllText(SettingsSources.File("DsaSettings", "WorkingSetTrim.cs"));
        Assert.Contains("if (Queue.Request(change))", trim);
    }
}

/// <summary>Finds the settings apps' sources from the test's output folder.</summary>
internal static class SettingsSources
{
    public static string File(params string[] parts) => Find(Path.Combine(parts), System.IO.File.Exists);

    public static string Dir(params string[] parts) => Find(Path.Combine(parts), Directory.Exists);

    private static string Find(string inSettings, Func<string, bool> exists)
    {
        string[] relative = { inSettings, Path.Combine("host", "platform", "windows", "settings", inSettings) };
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null; dir = dir.Parent)
        {
            var found = relative.Select(r => Path.Combine(dir.FullName, r)).FirstOrDefault(exists);
            if (found is not null)
            {
                return found;
            }
        }
        throw new FileNotFoundException(inSettings);
    }
}
