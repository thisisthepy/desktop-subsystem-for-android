using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>Gives the lines the test set for each command line, then ends, and records what
/// it was asked to run.</summary>
internal sealed class FakeLogStream : ILogStream
{
    public List<string> Calls { get; } = new();
    public List<string> Output { get; } = new();

    public async IAsyncEnumerable<string> LinesAsync(
        IReadOnlyList<string> args,
        [System.Runtime.CompilerServices.EnumeratorCancellation] CancellationToken cancel)
    {
        Calls.Add(string.Join(" ", args));
        foreach (var line in Output)
        {
            cancel.ThrowIfCancellationRequested();
            await Task.Yield();
            yield return line;
        }
    }
}

public class LogViewModelTests
{
    private static string Record(string tag, string message, string priority = "I") =>
        "{\"time\":\"2026-11-04 10:00:00.000\",\"time_ns\":1,\"buffer\":\"main\",\"priority\":\"" + priority +
        "\",\"pid\":12,\"tid\":13,\"uid\":1000,\"tag\":\"" + tag + "\",\"message\":\"" + message + "\"}";

    private static (LogViewModel, FakeLogStream, FakeCli) Make()
    {
        var stream = new FakeLogStream();
        var cli = new FakeCli();
        return (new LogViewModel(stream, new DsaService(cli), new KeyText()), stream, cli);
    }

    [Fact]
    public void Fr21_a_line_splits_into_the_page_columns_and_a_dropped_record_has_only_its_body()
    {
        var line = LogLine.Parse(Record("ActivityManager", "시작"))!;
        Assert.Equal("2026-11-04 10:00:00.000", line.TimeColumn);
        Assert.Equal("12", line.PidColumn);
        Assert.Equal("13", line.TidColumn);
        Assert.Equal("I", line.PriorityColumn);
        Assert.Equal("ActivityManager: 시작", line.BodyColumn);

        var dropped = LogLine.Parse("{\"time\":\"2026-11-04 10:00:00.000\",\"priority\":\"W\",\"dropped\":3}")!;
        Assert.Equal("", dropped.TimeColumn + dropped.PidColumn + dropped.TidColumn + dropped.PriorityColumn);
        Assert.Equal("--------- 3 records dropped", dropped.BodyColumn);
    }

    [Fact]
    public void Fr21_copying_selected_lines_gives_each_whole_line_in_page_order()
    {
        var (model, _, _) = Make();
        model.AddMany(new[] { Record("A", "one"), Record("B", "two"), Record("C", "three"), "{\"dropped\":3}" });
        var selected = new[] { model.Lines[3], model.Lines[0], model.Lines[2] };
        Assert.Equal(
            string.Join(Environment.NewLine,
                "2026-11-04 10:00:00.000    12    13 I A: one",
                "2026-11-04 10:00:00.000    12    13 I C: three",
                "--------- 3 records dropped"),
            model.CopyText(selected));
        Assert.Equal("", model.CopyText(Array.Empty<LogLine>()));
    }

    [Fact]
    public async Task Fr21_the_viewer_reads_the_log_at_the_chosen_priority_and_shows_each_line()
    {
        var (model, stream, _) = Make();
        stream.Output.Add(Record("ActivityManager", "Start proc"));
        stream.Output.Add("{\"dropped\":3}");
        stream.Output.Add("not json");
        await model.StartAsync();
        Assert.Equal("logcat --json *:I", stream.Calls.Single());
        Assert.Equal(2, model.Lines.Count);
        Assert.Equal("2026-11-04 10:00:00.000    12    13 I ActivityManager: Start proc", model.Lines[0].Text);
        Assert.Equal("--------- 3 records dropped", model.Lines[1].Text);
    }

    [Fact]
    public void Fr21_the_filter_keeps_lines_whose_tag_or_message_holds_the_text()
    {
        var (model, _, _) = Make();
        model.Add(Record("ActivityManager", "Start proc"));
        model.Add(Record("Zygote", "forked"));
        model.Add(Record("Other", "activitymanager said"));
        model.Filter = "activitymanager";
        Assert.Equal(new[] { "ActivityManager", "Other" }, model.Lines.Select(l => l.Tag));
        model.Filter = "";
        Assert.Equal(3, model.Lines.Count);
    }

    [Fact]
    public void Fr21_pausing_holds_the_lines_shown_and_resuming_shows_what_came_meanwhile()
    {
        var (model, _, _) = Make();
        model.Add(Record("A", "one"));
        model.Paused = true;
        model.Add(Record("B", "two"));
        model.Add(Record("C", "three"));
        Assert.Single(model.Lines);
        Assert.Equal(2, model.Waiting);
        Assert.Equal("Logs_Paused", model.PausedText);
        model.Paused = false;
        Assert.Equal(new[] { "A", "B", "C" }, model.Lines.Select(l => l.Tag));
        Assert.Equal(0, model.Waiting);
    }

    [Fact]
    public void Fr21_the_viewer_keeps_a_bounded_number_of_lines()
    {
        var (model, _, _) = Make();
        for (var i = 0; i < LogViewModel.MaxLines + 10; i++)
        {
            model.Add(Record("T", $"line {i}"));
        }
        Assert.Equal(LogViewModel.MaxLines, model.Lines.Count);
        Assert.Equal("line 10", model.Lines[0].Message);
    }

    [Fact]
    public async Task Fr21_changing_the_priority_reads_the_log_again_at_it()
    {
        var (model, stream, _) = Make();
        model.Priority = "W";
        await model.StartAsync();
        Assert.Equal("logcat --json *:W", stream.Calls.Last());
        model.Priority = "Q";
        Assert.Equal("W", model.Priority);
    }

    [Fact]
    public async Task Fr21_save_diagnostics_asks_dsa_for_the_archive_and_says_where_it_went()
    {
        var (model, _, cli) = Make();
        cli.Answer(@"logs collect -o C:\Users\me\diag.zip", "Saved the diagnostics");
        await model.SaveDiagnosticsAsync(@"C:\Users\me\diag.zip");
        Assert.Equal(@"logs collect -o C:\Users\me\diag.zip", cli.Calls.Single());
        Assert.Equal("Logs_Saved", model.Status);
        Assert.False(model.HasError);
        Assert.True(model.CanSave);

        cli.Fail(@"logs collect -o D:\x.zip", "dsa: cannot reach the DSA service");
        await model.SaveDiagnosticsAsync(@"D:\x.zip");
        Assert.True(model.HasError);
        Assert.Contains("Problem_ServiceUnreachable", model.Error);
    }

    [Fact]
    public void Fr21_a_burst_of_lines_is_shown_in_one_change_of_the_list()
    {
        // Hundreds of lines arrive at once; each one laying the page out made it crawl.
        var (model, _, _) = Make();
        var changes = 0;
        model.Lines.CollectionChanged += (_, _) => changes++;
        model.AddMany(Enumerable.Range(0, 300).Select(i => Record("T", $"line {i}")).ToList());
        Assert.Equal(1, changes);
        Assert.Equal(300, model.Lines.Count);
        Assert.Equal("line 299", model.Lines[^1].Message);
    }

    [Fact]
    public void Fr21_a_burst_past_the_limit_keeps_the_newest_lines()
    {
        var (model, _, _) = Make();
        model.AddMany(Enumerable.Range(0, LogViewModel.MaxLines + 50).Select(i => Record("T", $"line {i}")).ToList());
        Assert.Equal(LogViewModel.MaxLines, model.Lines.Count);
        Assert.Equal("line 50", model.Lines[0].Message);
    }
}
