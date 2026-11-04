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
    Stalled,
}

public sealed record StatusInfo(SessionState State, string? LastReason, string? UpdateNotice, string? Problem = null);

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
    string? LogDir,
    bool Clipboard = true,
    bool LinksInAndroid = false,
    bool DeveloperMode = false,
    int AdbPort = SettingsInfo.DefaultAdbPort)
{
    /// <summary>The loopback port adb connects to in developer mode unless the user picks
    /// another.</summary>
    public const int DefaultAdbPort = 58526;

    /// <summary>What to type to connect adb while developer mode is on.</summary>
    public string AdbConnect => $"adb connect 127.0.0.1:{AdbPort}";

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

/// <summary>How installing an APK ended: <see cref="Status"/> is <c>installed</c> or the
/// stable reason dsa gives (<c>incompatible</c>, <c>conflicting-signature</c>,
/// <c>invalid-apk</c>, <c>insufficient-storage</c>, <c>downgrade</c>, <c>blocked</c> or
/// <c>other</c>). <see cref="Detail"/> is what dsa said, in English.</summary>
public sealed record InstallReport(bool Installed, string Status, string? Package, string Detail);

/// <summary>A widget an app in Android offers: <see cref="Provider"/> names it to dsa, and the
/// sizes are in device-independent pixels.</summary>
public sealed record WidgetOffer(string Provider, string Package, string Label, byte[] PreviewPng,
    int DefaultWidthDp, int DefaultHeightDp, bool NeedsConfiguration);

/// <summary>A widget placed on the desktop, where its window is and how large, in
/// device-independent pixels.</summary>
public sealed record PlacedWidget(int Id, string Provider, string Label, int X, int Y, int WidthDp, int HeightDp);

/// <summary>The widgets on offer and the ones placed.</summary>
public sealed record WidgetList(IReadOnlyList<WidgetOffer> Offers, IReadOnlyList<PlacedWidget> Placed);
