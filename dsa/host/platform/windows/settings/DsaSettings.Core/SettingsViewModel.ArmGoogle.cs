using System.Globalization;

namespace Dsa.Settings.Core;

/// <summary>Google's ARM translator in the ARM apps section: offered beside a package the user
/// builds, fetched from Google only when the user agrees to Google's terms.</summary>
public sealed partial class SettingsViewModel
{
    private bool _fetchingGoogleArm;

    /// <summary>Whether the section offers Google's translator. An ARM64 computer's Android runs
    /// ARM apps itself, so dsa says it is not offered there, and a service that cannot say
    /// offers nothing.</summary>
    public bool ShowsGoogleArm => _arm?.GoogleFetchAvailable ?? false;

    public bool CanFetchGoogleArm => CanChange && ShowsGoogleArm;

    /// <summary>Whether the image is being downloaded and the translator installed now, which
    /// takes minutes.</summary>
    public bool IsFetchingGoogleArm => _fetchingGoogleArm;

    public string GoogleArmTermsUrl => DsaService.GoogleArmTermsUrl;

    /// <summary>What the confirmation says before anything is downloaded: where it comes from,
    /// and the terms the user accepts.</summary>
    public string GoogleArmNotice => string.Format(CultureInfo.CurrentCulture, _text.Get("ArmGoogle_Notice"),
        DsaService.GoogleArmSource, DsaService.GoogleArmTermsUrl);

    /// <summary>Fetches and installs Google's translator. The user has agreed to Google's terms
    /// in the confirmation. A refusal (a download whose hash differs, no image for this Android)
    /// leaves the section as the service last reported it, with the reason shown.</summary>
    public async Task FetchGoogleArmTranslatorAsync()
    {
        _busy = true;
        _fetchingGoogleArm = true;
        _error = null;
        ChangedAll();
        try
        {
            _arm = await _service.FetchGoogleArmTranslatorAsync().ConfigureAwait(true);
        }
        catch (Exception e)
        {
            _error = Format.Problem(_text, e);
        }
        finally
        {
            _busy = false;
            _fetchingGoogleArm = false;
            ChangedAll();
        }
    }
}
