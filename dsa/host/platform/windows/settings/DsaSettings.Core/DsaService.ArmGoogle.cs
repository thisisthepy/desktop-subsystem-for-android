namespace Dsa.Settings.Core;

/// <summary>Google's ARM translator: the user's computer downloads it from Google when the
/// user asks, and dsa installs it as a translation package. DSA does not ship it.</summary>
public sealed partial class DsaService
{
    /// <summary>Google's terms for what its SDK repository gives: the Android SDK License.</summary>
    public const string GoogleArmTermsUrl = "https://developer.android.com/studio/terms";

    /// <summary>Where the emulator image the translator comes from is downloaded.</summary>
    public const string GoogleArmSource = "https://dl.google.com/android/repository/sys-img/google_apis/";

    /// <summary>Downloads Google's emulator image for this Android, takes its ARM translator out
    /// and installs it, starting Android first when it is not running. The user agreed to
    /// Google's terms in the app first, so dsa does not ask again. It takes minutes: the image
    /// is about 2 GB.</summary>
    public async Task<ArmTranslationInfo> FetchGoogleArmTranslatorAsync() =>
        ReadArmTranslation(await RunJsonAsync("arm-translation", "fetch", "google", "--yes").ConfigureAwait(true));
}
