using System.Globalization;

namespace Dsa.Settings.Core;

/// <summary>Where Widevine stands, as the protected content section shows it.</summary>
public enum WidevineState
{
    /// <summary>No module is installed, and the running Android has none.</summary>
    NotInstalled,
    /// <summary>A module is installed and the running Android has it, or Android is not
    /// running.</summary>
    Installed,
    /// <summary>What is installed differs from what Android started with: a new module waits
    /// for the next start, or a removed one is still in use until then.</summary>
    RestartPending,
}

/// <summary>The protected content section of the Advanced page: install and remove Widevine,
/// which the user's computer takes from Google's emulator image only when the user agrees to
/// Google's terms, and what L3 means for streaming.</summary>
public sealed partial class SettingsViewModel
{
    private WidevineInfo? _widevine;
    private bool _installingWidevine;

    public WidevineInfo? Widevine => _widevine;

    public WidevineState WidevineState => _widevine switch
    {
        { RestartNeeded: true } => WidevineState.RestartPending,
        { Package: not null } => WidevineState.Installed,
        _ => WidevineState.NotInstalled,
    };

    public bool HasWidevinePackage => _widevine?.Package is not null;

    public bool CanInstallWidevine => CanChange && !HasWidevinePackage;

    public bool CanRemoveWidevine => CanChange && HasWidevinePackage;

    /// <summary>Whether the image is being downloaded and the module installed now, which takes
    /// minutes.</summary>
    public bool IsInstallingWidevine => _installingWidevine;

    public bool WidevineRestartNeeded => WidevineState == WidevineState.RestartPending;

    /// <summary>One line on whether Widevine is installed, and from which image.</summary>
    public string WidevineStatus
    {
        get
        {
            var package = _widevine?.Package;
            return (WidevineState, package) switch
            {
                (WidevineState.Installed, { } p) => DescribeWidevine("Widevine_Installed", p),
                (WidevineState.RestartPending, { } p) => DescribeWidevine("Widevine_InstalledPending", p),
                (WidevineState.RestartPending, null) => _text.Get("Widevine_RemovedPending"),
                _ => _text.Get("Widevine_NotInstalled"),
            };
        }
    }

    private string DescribeWidevine(string key, WidevinePackageInfo package) =>
        string.Format(CultureInfo.CurrentCulture, _text.Get(key), package.Version,
            package.AndroidMajor > 0 ? package.AndroidMajor.ToString(CultureInfo.CurrentCulture) : "?",
            package.Abi.Length > 0 ? package.Abi : "?");

    /// <summary>What the confirmation says before anything is downloaded: where it comes from,
    /// and the terms the user accepts.</summary>
    public string WidevineNotice => string.Format(CultureInfo.CurrentCulture, _text.Get("Widevine_Notice"),
        DsaService.WidevineSource, DsaService.WidevineTermsUrl);

    /// <summary>That streaming services send an L3 device standard definition at most, and where to
    /// watch in high definition.</summary>
    public string WidevineQualityNotice => _text.Get("Widevine_Quality");

    public string WidevineTermsUrl => DsaService.WidevineTermsUrl;

    /// <summary>Reads where Widevine stands. A service that cannot say leaves the section saying
    /// it is not installed.</summary>
    private async Task LoadWidevineAsync()
    {
        try
        {
            _widevine = await _service.WidevineAsync().ConfigureAwait(true);
        }
        catch (DsaException)
        {
            _widevine = null;
        }
        ChangedAll();
    }

    /// <summary>Takes Widevine from Google's image and installs it. The user has agreed to
    /// Google's terms in the confirmation. A refusal (no Widevine in Google's image for this
    /// Android, a download whose hash differs) leaves the section as the service last reported
    /// it, with the reason shown.</summary>
    public Task InstallWidevineAsync() => ChangeWidevineAsync(_service.InstallWidevineAsync, installing: true);

    public Task RemoveWidevineAsync() => ChangeWidevineAsync(_service.RemoveWidevineAsync, installing: false);

    private async Task ChangeWidevineAsync(Func<Task<WidevineInfo>> change, bool installing)
    {
        _busy = true;
        _installingWidevine = installing;
        _error = null;
        ChangedAll();
        try
        {
            _widevine = await change().ConfigureAwait(true);
        }
        catch (Exception e)
        {
            _error = Format.Problem(_text, e);
        }
        finally
        {
            _busy = false;
            _installingWidevine = false;
            ChangedAll();
        }
    }
}
