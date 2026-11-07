using System.Globalization;

namespace Dsa.Settings.Core;

/// <summary>The APK Installer window's title. It is the one text in that window that follows
/// the Windows UI language: Korean when the UI is Korean, English otherwise. Every other
/// text in the window stays English, so this lookup knows only this one string.</summary>
public static class ApkInstallerTitle
{
    public const string English = "APK Installer";
    public const string Korean = "APK 설치 관리자";

    public static string For(CultureInfo culture) =>
        culture.TwoLetterISOLanguageName == "ko" ? Korean : English;
}
