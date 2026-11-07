using System.Globalization;

namespace Dsa.Settings.Core;

/// <summary>Where Google Play stands, as the Google Play section shows it.</summary>
public enum GappsState
{
    /// <summary>No package is installed, and the running Android has none.</summary>
    NotInstalled,
    /// <summary>A package is installed and the running Android has it, or Android is not
    /// running.</summary>
    Installed,
    /// <summary>What is installed differs from what Android started with: a new package waits
    /// for the next start, or a removed one is still in use until then.</summary>
    RestartPending,
}

/// <summary>The Google Play section of the Advanced page: install and remove the GApps package
/// the service downloads, and how to register this device with Google.</summary>
public sealed partial class SettingsViewModel
{
    private GappsInfo? _gapps;
    private bool _installingGapps;

    public GappsInfo? Gapps => _gapps;

    public GappsState GappsState => _gapps switch
    {
        { RestartNeeded: true } => GappsState.RestartPending,
        { Package: not null } => GappsState.Installed,
        _ => GappsState.NotInstalled,
    };

    public bool HasGappsPackage => _gapps?.Package is not null;

    /// <summary>Install is offered only while no package is installed.</summary>
    public bool CanInstallGapps => CanChange && !HasGappsPackage;

    public bool CanRemoveGapps => CanChange && HasGappsPackage;

    /// <summary>Whether the package is being downloaded and installed now, which takes
    /// minutes.</summary>
    public bool IsInstallingGapps => _installingGapps;

    public bool GappsRestartNeeded => GappsState == GappsState.RestartPending;

    /// <summary>One line on whether Google Play is installed, and which package.</summary>
    public string GappsStatus
    {
        get
        {
            var package = _gapps?.Package;
            return (GappsState, package) switch
            {
                (GappsState.Installed, { } p) => Describe("Gapps_Installed", p),
                (GappsState.RestartPending, { } p) => Describe("Gapps_InstalledPending", p),
                (GappsState.RestartPending, null) => _text.Get("Gapps_RemovedPending"),
                _ => _text.Get("Gapps_NotInstalled"),
            };
        }
    }

    private string Describe(string key, GappsPackageInfo package) =>
        string.Format(CultureInfo.CurrentCulture, _text.Get(key), package.Version,
            package.AndroidVersion.Length > 0 ? package.AndroidVersion : "?",
            package.Arch.Length > 0 ? package.Arch : "?");

    /// <summary>Where the installed package came from: the LiteGapps file, or its address when
    /// the service names no file.</summary>
    public string GappsSource => _gapps?.Package is { } package
        ? string.Format(CultureInfo.CurrentCulture, _text.Get("Gapps_Source"),
            package.File.Length > 0 ? package.File : package.Source)
        : "";

    public bool HasGappsSource => GappsSource.Length > 0;

    /// <summary>The Google Services Framework Android ID the registration page asks for, when
    /// the running Android knows it.</summary>
    public string GappsDeviceId => _gapps?.Android?.DeviceId ?? "";

    public bool HasGappsDeviceId => GappsDeviceId.Length > 0;

    /// <summary>Whether the section says where the ID will show, because it has none yet.</summary>
    public bool ShowsGappsDeviceIdHint => !HasGappsDeviceId;

    /// <summary>The ID, or where it will show.</summary>
    public string GappsDeviceIdText => HasGappsDeviceId
        ? string.Format(CultureInfo.CurrentCulture, _text.Get("Gapps_DeviceId"), GappsDeviceId)
        : _text.Get("Gapps_NoDeviceId");

    public string GappsTermsUrl => _gapps?.TermsUrl ?? DsaService.DefaultGappsTermsUrl;

    public string GappsRegistrationUrl => _gapps?.RegistrationUrl ?? DsaService.DefaultGappsRegistrationUrl;

    /// <summary>Reads where Google Play stands. A service that cannot say leaves the section
    /// saying it is not installed.</summary>
    private async Task LoadGappsAsync()
    {
        try
        {
            _gapps = await _service.GappsAsync().ConfigureAwait(true);
        }
        catch (DsaException)
        {
            _gapps = null;
        }
        ChangedAll();
    }

    public Task InstallGappsAsync() => ChangeGappsAsync(_service.InstallGappsAsync, installing: true);

    public Task RemoveGappsAsync() => ChangeGappsAsync(_service.RemoveGappsAsync, installing: false);

    private async Task ChangeGappsAsync(Func<Task<GappsInfo>> change, bool installing)
    {
        _busy = true;
        _installingGapps = installing;
        _error = null;
        ChangedAll();
        try
        {
            _gapps = await change().ConfigureAwait(true);
        }
        catch (Exception e)
        {
            // The section stays as the service last reported it.
            _error = Format.Problem(_text, e);
        }
        finally
        {
            _busy = false;
            _installingGapps = false;
            ChangedAll();
        }
    }
}
