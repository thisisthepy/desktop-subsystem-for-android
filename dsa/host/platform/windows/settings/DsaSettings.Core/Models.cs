namespace Dsa.Settings.Core;

/// <summary>Where the user's Android is in its life, as the service reports it.</summary>
public enum SessionState
{
    Unknown,
    Stopped,
    Starting,
    Active,
    Dozing,
    Frozen,
    Stopping,
}

public sealed record StatusInfo(SessionState State, string? LastReason, string? UpdateNotice);

/// <summary>Which GPU the window manager renders on.</summary>
public enum RenderGpuKind
{
    /// <summary>The GPU that drives the primary monitor (the default).</summary>
    Display,
    Integrated,
    Discrete,
    /// <summary>The first GPU whose name contains <see cref="SettingsInfo.RenderGpu"/>.</summary>
    Named,
}

public sealed record SettingsInfo(
    string RenderGpu,
    bool Freeze,
    bool ProgramUpdates,
    bool ImageUpdates,
    bool ProgramUpdatesAvailable,
    bool ImageUpdatesAvailable,
    string Version,
    string? LogDir)
{
    public RenderGpuKind RenderGpuKind => RenderGpu.ToLowerInvariant() switch
    {
        "display" => RenderGpuKind.Display,
        "integrated" => RenderGpuKind.Integrated,
        "discrete" => RenderGpuKind.Discrete,
        _ => RenderGpuKind.Named,
    };
}

/// <summary>What a host process does for the user's Android.</summary>
public enum HostRole
{
    Vm,
    WindowManager,
    AppWindow,
}

public sealed record HostProcess(HostRole Role, uint Pid, ulong WorkingSetKb, ulong PrivateKb);

public sealed record GuestProcess(int Pid, string Name, ulong PssKb, ulong SwapPssKb);

public sealed record GuestMemoryInfo(
    ulong TotalKb,
    ulong FreeKb,
    ulong AvailableKb,
    ulong BuffersKb,
    ulong CachedKb,
    ulong AnonPagesKb,
    ulong ShmemKb,
    ulong SlabReclaimableKb,
    ulong SwapTotalKb,
    ulong SwapFreeKb,
    ulong ZramOriginalKb,
    ulong ZramUsedKb,
    ulong TotalPssKb,
    IReadOnlyList<GuestProcess> TopProcesses,
    ulong AgeSeconds);

public sealed record MemoryReport(GuestMemoryInfo? Guest, IReadOnlyList<HostProcess> Host, ulong HostTotalKb);

public sealed record GpuInfo(string Name, string Luid, bool PrimaryDisplay);
