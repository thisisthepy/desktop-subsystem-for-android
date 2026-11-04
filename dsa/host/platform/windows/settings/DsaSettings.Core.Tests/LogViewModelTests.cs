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
}
