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
    string? AndroidVersion = null,
    bool DeveloperMode = false,
    int AdbPort = SettingsInfo.DefaultAdbPort,
    bool InputTrace = false,
    bool ResizeTrace = false,
    int MemoryLimitMb = 0,
    bool ArmTranslation = true,
    bool StopWithoutAsking = false,
    bool AdvancedNetworking = false,
    NetworkInfo? Network = null,
    bool FileSharing = true,
    IReadOnlyList<string>? SharedFolders = null,
    bool AdbAutoConnect = true,
    AdbAutoConnectState AdbAutoConnectState = AdbAutoConnectState.Off)
{
    /// <summary>The Windows folders that can be shared with Android, in the order the service
    /// keeps them.</summary>
    public static readonly IReadOnlyList<string> AllFolders =
        new[] { "Documents", "Downloads", "Pictures", "Music", "Videos", "Desktop" };

    /// <summary>The folders shared while file sharing is on: every one unless the user chose
    /// fewer.</summary>
    public IReadOnlyList<string> Folders => SharedFolders ?? AllFolders;

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

/// <summary>Where attaching developer mode's port to this computer's adb server stands, as
/// the service reports it.</summary>
public enum AdbAutoConnectState
{
    /// <summary>Developer mode or auto-connect is off.</summary>
    Off,
    /// <summary>No adb server runs on this computer yet.</summary>
    WaitingForServer,
    /// <summary>Android is not running, or adb in Android has not answered yet.</summary>
    WaitingForAndroid,
    /// <summary>The adb server lists DSA.</summary>
    Attached,
    /// <summary>The user ran adb disconnect; DSA is attached again when the adb server
    /// restarts.</summary>
    Detached,
}

/// <summary>How Android reaches the network.</summary>
public enum NetworkMode
{
    /// <summary>The service did not say.</summary>
    Unknown,
    /// <summary>Behind a NAT network of its own.</summary>
    Nat,
    /// <summary>On this computer's network as a device of its own.</summary>
    Advanced,
    /// <summary>Android is not running, so it has no network yet.</summary>
    Off,
}

/// <summary>Android's network as the service reports it: the mode, the addresses (IPv4
/// first), the DNS servers, the gateway, and a problem when the network is not what the
/// setting asks for.</summary>
public sealed record NetworkInfo(
    NetworkMode Mode,
    IReadOnlyList<string> Addresses,
    IReadOnlyList<string> DnsServers,
    string Gateway,
    string? Problem);

/// <summary>An ARM translation package, as it describes itself.</summary>
public sealed record ArmPackageInfo(string Name, string Version, string Library, string License);

/// <summary>What the running Android says of ARM translation.</summary>
public sealed record ArmGuestInfo(bool Installed, bool Active, string Abilist);

/// <summary>Where an ARM translator comes from.</summary>
public enum ArmSource
{
    /// <summary>The default translator the image of this Android carries.</summary>
    Default,
    /// <summary>A package the user installed.</summary>
    User,
    /// <summary>Google's translator, which the user fetched.</summary>
    Google,
}

/// <summary>The ARM translator Android uses: where it comes from, its name, version and
/// licence.</summary>
public sealed record ArmTranslatorInfo(ArmSource Source, string Name, string Version, string License);

/// <summary>Whether apps built only for 64-bit ARM can run: the package the user installed,
/// the setting, what the running Android booted with, whether a restart is due, whether
/// Google's translator can be fetched here, the default package the image carries (only an
/// x86_64 image has one), and the translator Android uses from its next start.</summary>
public sealed record ArmTranslationInfo(
    ArmPackageInfo? Package,
    bool Enabled,
    bool Supported,
    ArmGuestInfo? Android,
    bool RestartNeeded,
    bool GoogleFetchAvailable = false,
    ArmPackageInfo? Default = null,
    ArmTranslatorInfo? Translator = null);

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
    ulong AgeSeconds,
    GuestKernelInfo? Kernel = null);

/// <summary>What the guest kernel holds for itself, as <c>dsa memory</c> reports it, in KiB.
/// <see cref="ReservedAtBootKb"/> is null when the guest could not count its memory.</summary>
public sealed record GuestKernelInfo(
    ulong SlabUnreclaimableKb,
    ulong KernelStackKb,
    ulong PageTablesKb,
    ulong VmallocUsedKb,
    ulong PercpuKb,
    ulong CmaTotalKb,
    ulong MemOnlineKb,
    ulong UnaccountedKb,
    ulong? ReservedAtBootKb);

/// <summary>The memory of the user's Android. <see cref="VmBeyondGuestKb"/> is what the VM
/// holds on the host beyond what Android uses, null until Android has reported.</summary>
public sealed record MemoryReport(GuestMemoryInfo? Guest, IReadOnlyList<HostProcess> Host, ulong HostTotalKb,
    ulong? VmBeyondGuestKb = null);

public sealed record GpuInfo(string Name, string Luid, bool PrimaryDisplay);

/// <summary>How installing an APK ended: <see cref="Status"/> is <c>installed</c> or the
/// stable reason dsa gives (<c>incompatible</c>, <c>conflicting-signature</c>,
/// <c>invalid-apk</c>, <c>insufficient-storage</c>, <c>downgrade</c>, <c>blocked</c> or
/// <c>other</c>). <see cref="Detail"/> is what dsa said, in English.</summary>
public sealed record InstallReport(bool Installed, string Status, string? Package, string Detail);

/// <summary>A widget an app in Android offers: <see cref="Provider"/> names it to dsa, and the
/// sizes are in device-independent pixels.</summary>
public sealed record WidgetOffer(string Provider, string Package, string Label, byte[] PreviewPng,
    int DefaultWidthDp, int DefaultHeightDp, bool NeedsConfiguration, string AppLabel = "");

/// <summary>A widget placed on the desktop, where its window is and how large, in
/// device-independent pixels.</summary>
public sealed record PlacedWidget(int Id, string Provider, string Label, string AppLabel, int X, int Y, int WidthDp, int HeightDp);

/// <summary>The widgets on offer and the ones placed.</summary>
public sealed record WidgetList(IReadOnlyList<WidgetOffer> Offers, IReadOnlyList<PlacedWidget> Placed);

/// <summary>An app in Android that keeps running with no window because it runs a foreground
/// service, with the notification the service keeps shown.</summary>
public sealed record BackgroundApp(string Package, string Label, string Title, string Text,
    IReadOnlyList<string> Actions, byte[] IconPng);

/// <summary>An app's active media session: what it plays, and whether it plays
/// (<c>playing</c>, <c>paused</c> or <c>stopped</c>).</summary>
public sealed record MediaInfo(string Package, string Label, string Title, string Artist, string Album,
    string Playback);

/// <summary>The apps that run in the background, and the media sessions.</summary>
public sealed record BackgroundList(IReadOnlyList<BackgroundApp> Apps, IReadOnlyList<MediaInfo> Media);
