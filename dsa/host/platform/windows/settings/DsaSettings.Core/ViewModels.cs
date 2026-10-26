using System.ComponentModel;
using System.Globalization;
using System.Runtime.CompilerServices;

namespace Dsa.Settings.Core;

/// <summary>Looks up a text in the user's language. The app reads its .resw resources; the
/// tests give the key back.</summary>
public interface ITextSource
{
    string Get(string key);
}

public abstract class ObservableObject : INotifyPropertyChanged
{
    public event PropertyChangedEventHandler? PropertyChanged;

    protected void Changed([CallerMemberName] string? name = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));

    protected void ChangedAll() => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(string.Empty));
}

public static class Format
{
    /// <summary>A size in KiB as MB below 1 GB and as GB above, in the user's number format.</summary>
    public static string Size(ulong kb, CultureInfo? culture = null)
    {
        culture ??= CultureInfo.CurrentCulture;
        double mb = kb / 1024.0;
        return mb >= 1024
            ? string.Format(culture, "{0:0.00} GB", mb / 1024.0)
            : string.Format(culture, "{0:0.0} MB", mb);
    }

    /// <summary>What went wrong, in the user's language, with what dsa said after it.</summary>
    public static string Problem(ITextSource text, Exception error)
    {
        if (error is DsaException dsa)
        {
            var key = dsa.Problem switch
            {
                Core.Problem.CliMissing => "Problem_CliMissing",
                Core.Problem.ServiceUnreachable => "Problem_ServiceUnreachable",
                Core.Problem.Unreadable => "Problem_Unreadable",
                _ => "Problem_Refused",
            };
            return $"{text.Get(key)} ({dsa.Message})";
        }
        return $"{text.Get("Problem_Refused")} ({error.Message})";
    }
}

/// <summary>The General page: the state of Android, starting and stopping it, and the last
/// error.</summary>
public sealed class GeneralViewModel : ObservableObject
{
    private readonly DsaService _service;
    private readonly ITextSource _text;
    private StatusInfo? _status;
    private bool _busy;
    private string? _error;

    public GeneralViewModel(DsaService service, ITextSource text)
    {
        _service = service;
        _text = text;
    }

    public SessionState State => _status?.State ?? SessionState.Unknown;
    public string StateText => _text.Get($"State_{State}");
    public string LastReason => _status?.LastReason ?? "";
    public bool HasLastReason => !string.IsNullOrEmpty(_status?.LastReason);
    public string UpdateNotice => _status?.UpdateNotice ?? "";
    public bool HasUpdateNotice => !string.IsNullOrEmpty(_status?.UpdateNotice);
    public bool IsBusy => _busy;
    public bool CanStart => !_busy && State == SessionState.Stopped;
    public bool CanStop => !_busy && State is SessionState.Starting or SessionState.Active
        or SessionState.Dozing or SessionState.Frozen;
    public string Error => _error ?? "";
    public bool HasError => _error is not null;

    private async Task RunAsync(Func<Task<StatusInfo>> action)
    {
        _busy = true;
        _error = null;
        ChangedAll();
        try
        {
            _status = await action().ConfigureAwait(true);
        }
        catch (Exception e)
        {
            _error = Format.Problem(_text, e);
        }
        finally
        {
            _busy = false;
            ChangedAll();
        }
    }

    public Task RefreshAsync() => RunAsync(_service.StatusAsync);

    /// <summary>Reads the state again without marking the page busy, as a timer does while
    /// the page is shown. Does nothing while a start or a stop is under way.</summary>
    public async Task PollAsync()
    {
        if (_busy)
        {
            return;
        }
        try
        {
            var status = await _service.StatusAsync().ConfigureAwait(true);
            if (_busy)
            {
                return;
            }
            _status = status;
            _error = null;
        }
        catch (Exception e)
        {
            _error = Format.Problem(_text, e);
        }
        ChangedAll();
    }

    public Task StartAsync() => RunAsync(_service.StartAsync);
    public Task StopAsync() => RunAsync(_service.StopAsync);
}

/// <summary>One choice of render GPU: what the list shows and the value the setting keeps.</summary>
public sealed record GpuChoice(string Label, string Value);

/// <summary>The settings the Display, Updates, Advanced and About pages show and change.</summary>
public sealed class SettingsViewModel : ObservableObject
{
    private readonly DsaService _service;
    private readonly ITextSource _text;
    private SettingsInfo? _settings;
    private IReadOnlyList<GpuInfo> _gpus = Array.Empty<GpuInfo>();
    private string? _error;
    private bool _busy;

    public SettingsViewModel(DsaService service, ITextSource text)
    {
        _service = service;
        _text = text;
    }

    public SettingsInfo? Settings => _settings;
    public bool Loaded => _settings is not null;
    public bool IsBusy => _busy;
    public string Error => _error ?? "";
    public bool HasError => _error is not null;
    public bool Freeze => _settings?.Freeze ?? false;
    public bool ProgramUpdates => _settings?.ProgramUpdates ?? false;
    public bool ImageUpdates => _settings?.ImageUpdates ?? false;
    public bool ProgramUpdatesAvailable => _settings?.ProgramUpdatesAvailable ?? false;
    public bool ImageUpdatesAvailable => _settings?.ImageUpdatesAvailable ?? false;
    public bool CanChangeProgramUpdates => Loaded && !_busy && ProgramUpdatesAvailable;
    public bool CanChangeImageUpdates => Loaded && !_busy && ImageUpdatesAvailable;
    public bool CanChange => Loaded && !_busy;
    public string ServiceVersion => _settings?.Version ?? "";
    public string LogDir => _settings?.LogDir ?? "";
    public bool HasLogDir => !string.IsNullOrEmpty(_settings?.LogDir);
    public string RenderGpu => _settings?.RenderGpu ?? "display";

    /// <summary>The GPU that drives the primary monitor, when it is known.</summary>
    public string DisplayGpuName => _gpus.FirstOrDefault(g => g.PrimaryDisplay)?.Name ?? "";

    /// <summary>The three kinds, then each GPU of this machine by name.</summary>
    public IReadOnlyList<GpuChoice> GpuChoices
    {
        get
        {
            var choices = new List<GpuChoice>
            {
                new(_text.Get("Gpu_Display"), "display"),
                new(_text.Get("Gpu_Integrated"), "integrated"),
                new(_text.Get("Gpu_Discrete"), "discrete"),
            };
            foreach (var gpu in _gpus)
            {
                if (!choices.Any(c => c.Value == gpu.Name))
                {
                    choices.Add(new GpuChoice(gpu.Name, gpu.Name));
                }
            }
            // A name chosen before that matches no GPU found now still shows as chosen.
            if (!choices.Any(c => string.Equals(c.Value, RenderGpu, StringComparison.OrdinalIgnoreCase)))
            {
                choices.Add(new GpuChoice(RenderGpu, RenderGpu));
            }
            return choices;
        }
    }

    public int SelectedGpuIndex
    {
        get
        {
            var choices = GpuChoices;
            for (var i = 0; i < choices.Count; i++)
            {
                if (string.Equals(choices[i].Value, RenderGpu, StringComparison.OrdinalIgnoreCase))
                {
                    return i;
                }
            }
            return 0;
        }
    }

    private async Task ChangeAsync(Func<Task<SettingsInfo>> change)
    {
        _busy = true;
        _error = null;
        ChangedAll();
        try
        {
            _settings = await change().ConfigureAwait(true);
        }
        catch (Exception e)
        {
            // The values stay as the service last reported them, so a switch moves back.
            _error = Format.Problem(_text, e);
        }
        finally
        {
            _busy = false;
            ChangedAll();
        }
    }

    public async Task LoadAsync()
    {
        await ChangeAsync(_service.SettingsAsync).ConfigureAwait(true);
        try
        {
            _gpus = await _service.GpusAsync().ConfigureAwait(true);
        }
        catch (DsaException)
        {
            // The list only helps to choose; the kinds still work without it.
            _gpus = Array.Empty<GpuInfo>();
        }
        ChangedAll();
    }

    public Task SetRenderGpuAsync(string value)
    {
        if (string.Equals(value, RenderGpu, StringComparison.OrdinalIgnoreCase) && Loaded)
        {
            return Task.CompletedTask;
        }
        return ChangeAsync(() => _service.SetRenderGpuAsync(value));
    }

    public Task SetFreezeAsync(bool on) =>
        on == Freeze && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetFreezeAsync(on));

    public Task SetProgramUpdatesAsync(bool on) =>
        on == ProgramUpdates && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetProgramUpdatesAsync(on));

    public Task SetImageUpdatesAsync(bool on) =>
        on == ImageUpdates && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetImageUpdatesAsync(on));
}

/// <summary>One line of the memory page: what it is, a detail, and its size.</summary>
public sealed record MemoryRow(string Label, string Detail, string Size);

/// <summary>The Memory page: the host processes that run Android, and Android's own
/// breakdown.</summary>
public sealed class MemoryViewModel : ObservableObject
{
    private readonly DsaService _service;
    private readonly ITextSource _text;
    private MemoryReport? _report;
    private string? _error;

    public MemoryViewModel(DsaService service, ITextSource text)
    {
        _service = service;
        _text = text;
    }

    public MemoryReport? Report => _report;
    public string Error => _error ?? "";
    public bool HasError => _error is not null;
    public string HostTotal => Format.Size(_report?.HostTotalKb ?? 0);
    public bool HostRunning => _report is { Host.Count: > 0 };
    /// <summary>Whether the report says no host process of the user's Android runs.</summary>
    public bool HostNotRunning => _report is not null && !HostRunning;
    public bool GuestReported => _report?.Guest is not null;
    public bool GuestNotReported => !GuestReported;
    public string GuestTotal => _report?.Guest is { } guest ? Format.Size(guest.TotalKb) : "";
    public string GuestAvailable => _report?.Guest is { } guest ? Format.Size(guest.AvailableKb) : "";

    /// <summary>How full the guest's memory is, from 0 to 100.</summary>
    public double GuestUsedPercent => _report?.Guest is { TotalKb: > 0 } guest
        ? 100.0 * (guest.TotalKb - Math.Min(guest.AvailableKb, guest.TotalKb)) / guest.TotalKb
        : 0;

    public string GuestAge => _report?.Guest is { } guest
        ? string.Format(CultureInfo.CurrentCulture, _text.Get("Memory_ReportAge"), guest.AgeSeconds)
        : "";

    public IReadOnlyList<MemoryRow> HostRows => (_report?.Host ?? Array.Empty<HostProcess>())
        .Select(p => new MemoryRow(
            _text.Get($"Role_{p.Role}"),
            string.Format(CultureInfo.CurrentCulture, _text.Get("Memory_ProcessDetail"), p.Pid, Format.Size(p.PrivateKb)),
            Format.Size(p.WorkingSetKb)))
        .ToList();

    /// <summary>Android's breakdown: the apps and the system, the caches, the kernel's
    /// reclaimable caches, shared memory and compressed swap, then the largest processes.</summary>
    public IReadOnlyList<MemoryRow> GuestRows
    {
        get
        {
            if (_report?.Guest is not { } guest)
            {
                return Array.Empty<MemoryRow>();
            }
            var rows = new List<MemoryRow>
            {
                new(_text.Get("Memory_Apps"), _text.Get("Memory_AppsDetail"), Format.Size(guest.TotalPssKb)),
                new(_text.Get("Memory_Cache"), _text.Get("Memory_CacheDetail"), Format.Size(guest.CachedKb + guest.BuffersKb)),
                new(_text.Get("Memory_Kernel"), _text.Get("Memory_KernelDetail"), Format.Size(guest.SlabReclaimableKb)),
                new(_text.Get("Memory_Shared"), "", Format.Size(guest.ShmemKb)),
                new(_text.Get("Memory_Zram"),
                    string.Format(CultureInfo.CurrentCulture, _text.Get("Memory_ZramDetail"), Format.Size(guest.ZramOriginalKb)),
                    Format.Size(guest.ZramUsedKb)),
                new(_text.Get("Memory_Free"), "", Format.Size(guest.FreeKb)),
            };
            return rows;
        }
    }

    public IReadOnlyList<MemoryRow> GuestProcessRows => (_report?.Guest?.TopProcesses ?? Array.Empty<GuestProcess>())
        .Select(p => new MemoryRow(
            p.Name,
            string.Format(CultureInfo.CurrentCulture, _text.Get("Memory_GuestProcessDetail"), p.Pid),
            Format.Size(p.PssKb)))
        .ToList();

    public async Task RefreshAsync()
    {
        try
        {
            _report = await _service.MemoryAsync().ConfigureAwait(true);
            _error = null;
        }
        catch (Exception e)
        {
            _error = Format.Problem(_text, e);
        }
        ChangedAll();
    }
}
