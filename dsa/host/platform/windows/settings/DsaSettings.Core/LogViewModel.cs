using System.Collections.Concurrent;
using System.Collections.ObjectModel;
using System.Globalization;
using System.Text.Json;

namespace Dsa.Settings.Core;

/// <summary>One record of Android's log, as <c>dsa logcat --json</c> prints it.</summary>
public sealed record LogLine(
    string Time,
    string Priority,
    string Buffer,
    string Tag,
    long Pid,
    long Tid,
    string Message,
    ulong Dropped)
{
    /// <summary>The record as logcat's threadtime format prints it.</summary>
    public string Text => Dropped > 0
        ? $"--------- {Dropped} records dropped"
        : string.Format(CultureInfo.InvariantCulture, "{0} {1,5} {2,5} {3} {4}: {5}", Time, Pid, Tid, Priority, Tag, Message);

    // The page's columns. A record that stands for dropped records has only its body.
    public string TimeColumn => Dropped > 0 ? "" : Time;

    public string PidColumn => Dropped > 0 ? "" : Pid.ToString(CultureInfo.InvariantCulture);

    public string TidColumn => Dropped > 0 ? "" : Tid.ToString(CultureInfo.InvariantCulture);

    public string PriorityColumn => Dropped > 0 ? "" : Priority;

    public string BodyColumn => Dropped > 0 ? Text : $"{Tag}: {Message}";

    /// <summary>Reads one line of <c>dsa logcat --json</c>; null for a line that is not a record.</summary>
    public static LogLine? Parse(string json)
    {
        try
        {
            using var document = JsonDocument.Parse(json);
            var root = document.RootElement;
            if (root.ValueKind != JsonValueKind.Object)
            {
                return null;
            }
            string Str(string name) =>
                root.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.String ? v.GetString() ?? "" : "";
            long Number(string name) =>
                root.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.Number ? v.GetInt64() : 0;
            ulong dropped = root.TryGetProperty("dropped", out var d) && d.ValueKind == JsonValueKind.Number
                ? d.GetUInt64()
                : 0;
            return new LogLine(Str("time"), Str("priority"), Str("buffer"), Str("tag"), Number("pid"),
                Number("tid"), Str("message"), dropped);
        }
        catch (JsonException)
        {
            return null;
        }
    }
}

/// <summary>Runs a dsa command whose output goes on until it is stopped, line by line.</summary>
public interface ILogStream
{
    IAsyncEnumerable<string> LinesAsync(IReadOnlyList<string> args, CancellationToken cancel);
}

/// <summary>The Android log page: Android's log as it comes, filtered by priority and by
/// text, which can be paused, and the button that saves the diagnostics. Android sends its log
/// only while this page reads it; nothing is written to disk but the diagnostics archive the
/// user saves.</summary>
public sealed class LogViewModel : ObservableObject
{
    /// <summary>The most lines the page keeps. Older ones are dropped.</summary>
    public const int MaxLines = 5000;

    /// <summary>How often lines read from Android are shown.</summary>
    public static readonly TimeSpan FlushInterval = TimeSpan.FromMilliseconds(100);

    /// <summary>The priorities the page offers, least first, as logcat's letters.</summary>
    public static readonly IReadOnlyList<string> Priorities = new[] { "V", "D", "I", "W", "E", "F" };

    private readonly ILogStream _stream;
    private readonly DsaService _service;
    private readonly ITextSource _text;
    private readonly List<LogLine> _all = new();
    private CancellationTokenSource? _reading;
    private string _filter = "";
    private string _priority = "I";
    private bool _paused;
    private int _waiting;
    private string? _error;
    private string? _status;
    private bool _saving;

    public LogViewModel(ILogStream stream, DsaService service, ITextSource text)
    {
        _stream = stream;
        _service = service;
        _text = text;
    }

    /// <summary>The lines shown: those that pass the filter, as of the last time the page was
    /// not paused.</summary>
    public LogLines Lines { get; } = new();

    /// <summary>What copying the selected lines puts on the clipboard: each whole line as the
    /// log prints it, one per line, in the order the page shows them, whatever order they
    /// were selected in.</summary>
    public string CopyText(IEnumerable<LogLine> selected)
    {
        var chosen = new HashSet<LogLine>(selected, ReferenceEqualityComparer.Instance);
        return string.Join(Environment.NewLine, Lines.Where(chosen.Contains).Select(l => l.Text));
    }

    public bool IsReading => _reading is not null;
    public string Error => _error ?? "";
    public bool HasError => _error is not null;
    public string Status => _status ?? "";
    public bool HasStatus => _status is not null;
    public bool IsSaving => _saving;
    public bool CanSave => !_saving;

    /// <summary>Lines that came while paused and are not shown yet.</summary>
    public int Waiting => _waiting;
    public string PausedText => string.Format(CultureInfo.CurrentCulture, _text.Get("Logs_Paused"), _waiting);

    /// <summary>Shows only lines whose tag or message holds this text, ignoring case.</summary>
    public string Filter
    {
        get => _filter;
        set
        {
            if (_filter == value)
            {
                return;
            }
            _filter = value ?? "";
            Changed();
            if (!_paused)
            {
                Rebuild();
            }
        }
    }

    /// <summary>The least priority read, as a logcat letter. Android sends only what passes,
    /// so changing it reads the log again.</summary>
    public string Priority
    {
        get => _priority;
        set
        {
            if (_priority == value || !Priorities.Contains(value))
            {
                return;
            }
            _priority = value;
            Changed();
            if (IsReading)
            {
                _ = StartAsync();
            }
        }
    }

    /// <summary>While paused the lines shown stay as they are; new ones wait, and show when the
    /// page goes on.</summary>
    public bool Paused
    {
        get => _paused;
        set
        {
            if (_paused == value)
            {
                return;
            }
            _paused = value;
            if (!value)
            {
                _waiting = 0;
                Rebuild();
            }
            ChangedAll();
        }
    }

    /// <summary>The dsa command line that reads the log at the chosen priority.</summary>
    public static IReadOnlyList<string> Arguments(string priority) =>
        new[] { "logcat", "--json", $"*:{priority}" };

    private bool Passes(LogLine line) =>
        _filter.Length == 0
        || line.Dropped > 0
        || line.Tag.Contains(_filter, StringComparison.OrdinalIgnoreCase)
        || line.Message.Contains(_filter, StringComparison.OrdinalIgnoreCase);

    private void Rebuild() => Lines.ReplaceAll(_all.Where(Passes));

    /// <summary>Takes one line of dsa's output.</summary>
    public void Add(string output) => AddMany(new[] { output });

    /// <summary>Takes several lines of dsa's output and shows them in one change of the list.
    /// Android sends its log in bursts of hundreds of lines; adding and scrolling to each one
    /// laid the page out hundreds of times a second and made it crawl.</summary>
    public void AddMany(IReadOnlyCollection<string> outputs)
    {
        var added = 0;
        foreach (var output in outputs)
        {
            var line = LogLine.Parse(output);
            if (line is null)
            {
                continue;
            }
            _all.Add(line);
            added++;
        }
        if (added == 0)
        {
            return;
        }
        var trimmed = _all.Count > MaxLines;
        if (trimmed)
        {
            _all.RemoveRange(0, _all.Count - MaxLines);
        }
        if (_paused)
        {
            _waiting += added;
            Changed(nameof(Waiting));
            Changed(nameof(PausedText));
            return;
        }
        if (trimmed || added > 1)
        {
            Rebuild();
        }
        else if (Passes(_all[^1]))
        {
            Lines.Add(_all[^1]);
        }
    }

    /// <summary>Reads the log until <see cref="Stop"/>, starting over when already reading.</summary>
    public async Task StartAsync()
    {
        Stop();
        var reading = new CancellationTokenSource();
        _reading = reading;
        _all.Clear();
        _waiting = 0;
        _error = null;
        Lines.Clear();
        ChangedAll();
        try
        {
            // The log is read off the page's thread and shown at most ten times a second, one
            // change of the list each time, so a burst costs one layout instead of one a line.
            var queue = new ConcurrentQueue<string>();
            var stream = _stream;
            var priority = _priority;
            var pump = Task.Run(async () =>
            {
                await foreach (var output in stream.LinesAsync(Arguments(priority), reading.Token).ConfigureAwait(false))
                {
                    queue.Enqueue(output);
                }
            });
            var batch = new List<string>();
            while (true)
            {
                var finished = pump.IsCompleted;
                batch.Clear();
                while (queue.TryDequeue(out var output))
                {
                    batch.Add(output);
                }
                if (reading.IsCancellationRequested)
                {
                    break;
                }
                AddMany(batch);
                if (finished)
                {
                    await pump.ConfigureAwait(true);
                    break;
                }
                await Task.WhenAny(pump, Task.Delay(FlushInterval, reading.Token)).ConfigureAwait(true);
            }
        }
        catch (OperationCanceledException)
        {
        }
        catch (Exception e)
        {
            if (!reading.IsCancellationRequested)
            {
                _error = Format.Problem(_text, e);
            }
        }
        finally
        {
            if (ReferenceEquals(_reading, reading))
            {
                _reading = null;
            }
            reading.Dispose();
            ChangedAll();
        }
    }

    /// <summary>Stops reading, so Android stops sending its log.</summary>
    public void Stop()
    {
        var reading = _reading;
        _reading = null;
        if (reading is not null)
        {
            try
            {
                reading.Cancel();
            }
            catch (ObjectDisposedException)
            {
            }
        }
        Changed(nameof(IsReading));
    }

    /// <summary>Saves the diagnostics of Android and of DSA to <paramref name="path"/>, as one
    /// zip archive.</summary>
    public async Task SaveDiagnosticsAsync(string path)
    {
        _saving = true;
        _status = _text.Get("Logs_Saving");
        _error = null;
        ChangedAll();
        try
        {
            await _service.CollectDiagnosticsAsync(path).ConfigureAwait(true);
            _status = string.Format(CultureInfo.CurrentCulture, _text.Get("Logs_Saved"), path);
        }
        catch (Exception e)
        {
            _status = null;
            _error = Format.Problem(_text, e);
        }
        finally
        {
            _saving = false;
            ChangedAll();
        }
    }
}

/// <summary>The lines a log page shows. Replacing them all is one change of the list, so the
/// list lays itself out once, not once a line.</summary>
public sealed class LogLines : ObservableCollection<LogLine>
{
    public void ReplaceAll(IEnumerable<LogLine> lines)
    {
        CheckReentrancy();
        Items.Clear();
        foreach (var line in lines)
        {
            Items.Add(line);
        }
        OnPropertyChanged(new System.ComponentModel.PropertyChangedEventArgs(nameof(Count)));
        OnPropertyChanged(new System.ComponentModel.PropertyChangedEventArgs("Item[]"));
        OnCollectionChanged(new System.Collections.Specialized.NotifyCollectionChangedEventArgs(
            System.Collections.Specialized.NotifyCollectionChangedAction.Reset));
    }
}
