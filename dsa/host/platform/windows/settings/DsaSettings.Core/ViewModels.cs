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
public sealed partial class GeneralViewModel : ObservableObject
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
    /// <summary>Why Android last stopped, as a short sentence in the user's language.</summary>
    public string LastReason => HasLastReason ? _text.Get(StopReasonText.KeyFor(_status!.LastReason!)) : "";
    public bool HasLastReason => !string.IsNullOrEmpty(_status?.LastReason);
    /// <summary>The service's own words for it, shown only when the user asks for the details.
    /// A stop on request has none worth showing.</summary>
    public string LastReasonDetail => HasLastReasonDetail ? _status!.LastReason! : "";
    public bool HasLastReasonDetail => HasLastReason && StopReasonText.KeyFor(_status!.LastReason!) != StopReasonText.Requested;
    public string Problem => _status?.Problem ?? "";
    public bool HasProblem => !string.IsNullOrEmpty(_status?.Problem);
    public string UpdateNotice => _status?.UpdateNotice ?? "";
    public bool HasUpdateNotice => !string.IsNullOrEmpty(_status?.UpdateNotice);
    public bool IsBusy => _busy;
    public bool CanStart => !_busy && State == SessionState.Stopped;
    public bool CanStop => !_busy && State is SessionState.Starting or SessionState.Stalled or SessionState.Active
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
public sealed partial class SettingsViewModel : ObservableObject
{
    private readonly DsaService _service;
    private readonly ITextSource _text;
    private SettingsInfo? _settings;
    private ArmTranslationInfo? _arm;
    private IReadOnlyList<GpuInfo> _gpus = Array.Empty<GpuInfo>();
    private string? _error;
    private bool _busy;
    private bool _administrator;

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
    /// <summary>Whether web links apps in Android open go to Android apps instead of the
    /// default browser.</summary>
    public bool LinksInAndroid => _settings?.LinksInAndroid ?? false;
    /// <summary>Whether text copied on Windows can be pasted in Android, and the reverse.</summary>
    public bool Clipboard => _settings?.Clipboard ?? true;
    /// <summary>Whether the chosen Windows folders show in Android's shared storage.</summary>
    public bool FileSharing => _settings?.FileSharing ?? true;
    /// <summary>Whether the folder choices can be changed: only while sharing is on.</summary>
    public bool CanChooseFolders => CanChange && FileSharing;
    /// <summary>Whether <paramref name="folder"/> is shared while file sharing is on.</summary>
    public bool IsFolderShared(string folder) =>
        (_settings?.Folders ?? SettingsInfo.AllFolders).Contains(folder);
    public bool ShareDocuments => IsFolderShared("Documents");
    public bool ShareDownloads => IsFolderShared("Downloads");
    public bool SharePictures => IsFolderShared("Pictures");
    public bool ShareMusic => IsFolderShared("Music");
    public bool ShareVideos => IsFolderShared("Videos");
    public bool ShareDesktop => IsFolderShared("Desktop");
    /// <summary>Whether adb reaches Android through the loopback port.</summary>
    public bool DeveloperMode => _settings?.DeveloperMode ?? false;
    public int AdbPort => _settings?.AdbPort ?? SettingsInfo.DefaultAdbPort;
    /// <summary>The command that connects adb, shown while developer mode is on.</summary>
    public string AdbConnect => _settings?.AdbConnect ?? $"adb connect 127.0.0.1:{SettingsInfo.DefaultAdbPort}";
    /// <summary>Whether developer mode's port is attached to this PC's adb server by itself, so
    /// adb and Android Studio list Android without adb connect.</summary>
    public bool AdbAutoConnect => _settings?.AdbAutoConnect ?? true;
    /// <summary>Where auto-connect stands, as the service last reported it.</summary>
    public AdbAutoConnectState AdbAutoConnectState => _settings?.AdbAutoConnectState ?? AdbAutoConnectState.Off;
    /// <summary>Where auto-connect stands, in the user's language.</summary>
    public string AdbAutoConnectStateText => _text.Get($"AdbAutoConnect_{AdbAutoConnectState}");
    /// <summary>Whether Android uses an installed ARM translation package from its next
    /// start.</summary>
    public bool ArmTranslation => _settings?.ArmTranslation ?? true;
    /// <summary>Whether apps built only for 64-bit ARM can be installed and run.</summary>
    public bool ArmSupported => _arm?.Supported ?? false;
    public bool HasArmPackage => _arm?.Package is not null;
    public bool CanRemoveArmTranslation => CanChange && HasArmPackage;
    /// <summary>Whether the image carries a default translator. Only an x86_64 image does; an
    /// arm64 one runs ARM apps itself.</summary>
    public bool HasArmDefault => _arm?.Default is not null;
    /// <summary>The remove card shows only where there is no default translator: where there
    /// is one, removing the package is going back to it, which its own card does.</summary>
    public bool ShowsArmRemove => !HasArmDefault;
    /// <summary>Whether the default translator can be chosen again: a package is installed in
    /// its place.</summary>
    public bool CanUseDefaultArmTranslator => CanChange && HasArmDefault && HasArmPackage;
    /// <summary>What the default translator is, and what going back to it does.</summary>
    public string ArmDefaultDescription => _arm?.Default is { } d
        ? string.Format(System.Globalization.CultureInfo.CurrentCulture, _text.Get("Arm_DefaultDescription"),
            d.Name, d.Version, d.License.Length > 0 ? d.License : _text.Get("Arm_NoLicense"))
        : "";
    public bool ArmRestartNeeded => _arm?.RestartNeeded ?? false;
    /// <summary>One line on whether ARM apps run, and with what.</summary>
    public string ArmStatus
    {
        get
        {
            var culture = System.Globalization.CultureInfo.CurrentCulture;
            if (_arm?.Translator is { } translator)
            {
                var license = translator.License.Length > 0 ? translator.License : _text.Get("Arm_NoLicense");
                var template = translator.Source == ArmSource.Default
                    ? _text.Get("Arm_SupportedDefault")
                    : _text.Get("Arm_Supported");
                return string.Format(culture, template, translator.Name, translator.Version, license);
            }
            if (_arm is { Enabled: false, Package: { } package })
            {
                return string.Format(culture, _text.Get("Arm_TurnedOff"), package.Name, package.Version);
            }
            if (_arm is { Enabled: false, Default: { } builtIn })
            {
                return string.Format(culture, _text.Get("Arm_DefaultTurnedOff"), builtIn.Name, builtIn.Version);
            }
            return _text.Get("Arm_NotSupported");
        }
    }
    /// <summary>The ABIs the running Android installs apps for, or that it is not running.</summary>
    public string ArmAbis => _arm?.Android is { } android
        ? string.Format(System.Globalization.CultureInfo.CurrentCulture, _text.Get("Arm_Abis"),
            android.Abilist.Length > 0 ? android.Abilist : "?")
        : _text.Get("Arm_NotRunning");
    public bool ProgramUpdates => _settings?.ProgramUpdates ?? false;
    public bool ImageUpdates => _settings?.ImageUpdates ?? false;
    public bool ProgramUpdatesAvailable => _settings?.ProgramUpdatesAvailable ?? false;
    public bool ImageUpdatesAvailable => _settings?.ImageUpdatesAvailable ?? false;
    /// <summary>Whether the app runs as an administrator (an elevated process). The update
    /// settings apply to every user, so the service lets only an administrator change
    /// them.</summary>
    public bool IsAdministrator
    {
        get => _administrator;
        set
        {
            _administrator = value;
            ChangedAll();
        }
    }

    public bool CanChangeProgramUpdates => Loaded && !_busy && ProgramUpdatesAvailable && _administrator;
    public bool CanChangeImageUpdates => Loaded && !_busy && ImageUpdatesAvailable && _administrator;
    /// <summary>Whether a switch can be changed only by running dsa as an administrator.</summary>
    public bool ProgramUpdatesNeedAdministrator => Loaded && !_busy && ProgramUpdatesAvailable && !_administrator;
    public bool ImageUpdatesNeedAdministrator => Loaded && !_busy && ImageUpdatesAvailable && !_administrator;
    public bool NeedsAdministrator => Loaded && !_administrator && (ProgramUpdatesAvailable || ImageUpdatesAvailable);
    /// <summary>Whether the newest version can be installed now; any user may ask.</summary>
    public bool CanUpdateNow => Loaded && !_busy && ProgramUpdatesAvailable;
    public bool CanChange => Loaded && !_busy;
    public string ServiceVersion => _settings?.Version ?? "";
    public string AndroidVersion => _settings?.AndroidVersion ?? "";
    public bool HasAndroidVersion => !string.IsNullOrEmpty(_settings?.AndroidVersion);
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

    /// <summary>Reads where ARM translation stands. A service that cannot say leaves the
    /// section saying ARM apps are not supported.</summary>
    private async Task LoadArmTranslationAsync()
    {
        try
        {
            _arm = await _service.ArmTranslationAsync().ConfigureAwait(true);
        }
        catch (DsaException)
        {
            _arm = null;
        }
        ChangedAll();
    }

    public async Task SetArmTranslationAsync(bool on)
    {
        if (on == ArmTranslation && Loaded)
        {
            return;
        }
        await ChangeAsync(() => _service.SetArmTranslationAsync(on)).ConfigureAwait(true);
        await LoadArmTranslationAsync().ConfigureAwait(true);
    }

    /// <summary>Goes back to the image's default translator: the package installed in its
    /// place is removed, from the next start of Android.</summary>
    public async Task UseDefaultArmTranslatorAsync()
    {
        _busy = true;
        _error = null;
        ChangedAll();
        try
        {
            _arm = await _service.UseDefaultArmTranslatorAsync().ConfigureAwait(true);
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

    public async Task RemoveArmTranslationAsync()
    {
        _busy = true;
        _error = null;
        ChangedAll();
        try
        {
            _arm = await _service.RemoveArmTranslationAsync().ConfigureAwait(true);
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

    public async Task LoadAsync()
    {
        await ChangeAsync(_service.SettingsAsync).ConfigureAwait(true);
        await LoadArmTranslationAsync().ConfigureAwait(true);
        await LoadGappsAsync().ConfigureAwait(true);
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

    public Task SetLinksInAndroidAsync(bool on) =>
        on == LinksInAndroid && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetLinksInAndroidAsync(on));

    public Task SetClipboardAsync(bool on) =>
        on == Clipboard && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetClipboardAsync(on));

    public Task SetFileSharingAsync(bool on) =>
        on == FileSharing && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetFileSharingAsync(on));

    /// <summary>Shares <paramref name="folder"/> or stops sharing it, keeping the other
    /// choices. A folder that cannot be shared changes nothing.</summary>
    public Task SetFolderSharedAsync(string folder, bool shared)
    {
        if (!SettingsInfo.AllFolders.Contains(folder) || (shared == IsFolderShared(folder) && Loaded))
        {
            ChangedAll();
            return Task.CompletedTask;
        }
        var wanted = SettingsInfo.AllFolders
            .Where(name => name == folder ? shared : IsFolderShared(name))
            .ToList();
        return ChangeAsync(() => _service.SetSharedFoldersAsync(wanted));
    }

    /// <summary>Opens Android's shared storage in Explorer. A failure shows as the page's
    /// error.</summary>
    public async Task OpenAndroidFilesAsync()
    {
        _error = null;
        ChangedAll();
        try
        {
            await _service.OpenAndroidFilesAsync().ConfigureAwait(true);
        }
        catch (Exception e)
        {
            _error = Format.Problem(_text, e);
        }
        ChangedAll();
    }

    public Task SetDeveloperModeAsync(bool on) =>
        on == DeveloperMode && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetDeveloperModeAsync(on));

    public Task SetAdbAutoConnectAsync(bool on) =>
        on == AdbAutoConnect && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetAdbAutoConnectAsync(on));

    /// <summary>Reads where auto-connect stands again, as a timer does while the page is shown
    /// and developer mode is on. Only the state is taken, so a value being typed is kept.</summary>
    public async Task PollAdbAutoConnectAsync()
    {
        if (_busy || _settings is not { DeveloperMode: true })
        {
            return;
        }
        try
        {
            var now = await _service.SettingsAsync().ConfigureAwait(true);
            if (_busy || _settings is null || now.AdbAutoConnectState == _settings.AdbAutoConnectState)
            {
                return;
            }
            _settings = _settings with { AdbAutoConnectState = now.AdbAutoConnectState };
            Changed(nameof(AdbAutoConnectState));
            Changed(nameof(AdbAutoConnectStateText));
        }
        catch (Exception)
        {
            // The state stays as last read; the next tick asks again.
        }
    }

    /// <summary>Picks the loopback port adb connects to. Text that is not a port from 1 to
    /// 65535 changes nothing.</summary>
    public Task SetAdbPortAsync(string text)
    {
        if (!int.TryParse(text.Trim(), System.Globalization.NumberStyles.None,
                System.Globalization.CultureInfo.InvariantCulture, out var port) || port is < 1 or > 65535)
        {
            ChangedAll();
            return Task.CompletedTask;
        }
        return port == AdbPort && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetAdbPortAsync(port));
    }

    public Task SetProgramUpdatesAsync(bool on) =>
        on == ProgramUpdates && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetProgramUpdatesAsync(on));

    public Task SetImageUpdatesAsync(bool on) =>
        on == ImageUpdates && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetImageUpdatesAsync(on));
}

/// <summary>How the install bar looks.</summary>
public enum InstallSeverity
{
    Informational,
    Success,
    Error,
}

/// <summary>APK files and split-APK bundles dropped on the window: each is installed in Android
/// in turn, and the bar at the top of the window says how it went.</summary>
public sealed class InstallViewModel : ObservableObject
{
    private readonly DsaService _service;
    private readonly ITextSource _text;
    private bool _busy;
    private bool _open;
    private InstallSeverity _severity;
    private string _title = "";
    private string _message = "";

    public InstallViewModel(DsaService service, ITextSource text)
    {
        _service = service;
        _text = text;
    }

    public bool IsBusy => _busy;
    /// <summary>Whether the bar shows; its close button sets it to false.</summary>
    public bool IsOpen
    {
        get => _open;
        set
        {
            _open = value;
            Changed();
        }
    }
    public InstallSeverity Severity => _severity;
    public string Title => _title;
    public string Message => _message;

    /// <summary>The extensions of what goes to the APK Installer: APKs, the split-APK bundles
    /// it installs, and App Bundles, for which it says why it does not.</summary>
    public static readonly string[] InstallExtensions = { ".apk", ".xapk", ".apks", ".apkm", ".aab" };

    /// <summary>Whether <paramref name="path"/> names an APK file or a bundle of APKs.</summary>
    public static bool IsApk(string path) =>
        InstallExtensions.Any(extension => path.EndsWith(extension, StringComparison.OrdinalIgnoreCase));

    /// <summary>Whether a drop of <paramref name="paths"/> would install anything, so the
    /// window shows a drop is welcome.</summary>
    public bool Accepts(IEnumerable<string>? paths) => !_busy && paths is not null && paths.Any(IsApk);

    private void Show(InstallSeverity severity, string title, string message)
    {
        _severity = severity;
        _title = title;
        _message = message;
        _open = true;
        ChangedAll();
    }

    /// <summary>Installs every APK of <paramref name="paths"/>, one after the other, and says
    /// how each went. Files that are not APKs are named and left alone.</summary>
    public async Task DropAsync(IReadOnlyList<string> paths)
    {
        if (_busy)
        {
            return;
        }
        var apks = paths.Where(IsApk).ToList();
        var others = paths.Where(p => !IsApk(p)).Select(p => Path.GetFileName(p)).ToList();
        if (apks.Count == 0)
        {
            Show(InstallSeverity.Error, _text.Get("Install_FailedTitle"),
                string.Format(CultureInfo.CurrentCulture, _text.Get("Install_NotApk"), string.Join(", ", others)));
            return;
        }
        _busy = true;
        var lines = new List<string>();
        var failed = false;
        try
        {
            foreach (var path in apks)
            {
                var name = Path.GetFileName(path);
                Show(InstallSeverity.Informational, _text.Get("Install_BusyTitle"),
                    string.Format(CultureInfo.CurrentCulture, _text.Get("Install_Busy"), name));
                try
                {
                    var report = await _service.InstallAsync(path).ConfigureAwait(true);
                    if (report.Installed)
                    {
                        lines.Add(string.Format(CultureInfo.CurrentCulture, _text.Get("Install_Installed"),
                            name, report.Package ?? ""));
                    }
                    else
                    {
                        failed = true;
                        lines.Add(string.Format(CultureInfo.CurrentCulture, _text.Get("Install_Refused"),
                            name, _text.Get($"Install_Reason_{report.Status}"), report.Detail));
                    }
                }
                catch (Exception e)
                {
                    failed = true;
                    lines.Add(string.Format(CultureInfo.CurrentCulture, _text.Get("Install_NotSent"),
                        name, Format.Problem(_text, e)));
                }
            }
            if (others.Count > 0)
            {
                lines.Add(string.Format(CultureInfo.CurrentCulture, _text.Get("Install_NotApk"), string.Join(", ", others)));
            }
        }
        finally
        {
            _busy = false;
        }
        Show(failed ? InstallSeverity.Error : InstallSeverity.Success,
            _text.Get(failed ? "Install_FailedTitle" : "Install_DoneTitle"),
            string.Join("\n", lines));
    }
}

/// <summary>One line of the memory page: what it is, a detail, and its size.</summary>
public sealed record MemoryRow(string Label, string Detail, string Size);

/// <summary>The Memory page: the host processes that run Android, and Android's own
/// breakdown.</summary>
public sealed partial class MemoryViewModel : ObservableObject
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
