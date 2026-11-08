using System.Globalization;

namespace Dsa.Settings.Core;

/// <summary>The Display page's switch that turns Windows' three- and four-finger touch gestures
/// off while a DSA app window is in front, so apps get every finger.</summary>
public sealed partial class SettingsViewModel
{
    /// <summary>The first Windows build with those gestures: Windows 11.</summary>
    public const int TouchGesturesBuild = 22000;

    /// <summary>Where Windows Settings changes the gestures for every app.</summary>
    public const string TouchSettingsUri = "ms-settings:devices-touch";

    /// <summary>The Windows build this app runs on, which says whether the gestures exist while
    /// the window manager has not said.</summary>
    public int WindowsBuild { get; init; } = Environment.OSVersion.Version.Build;

    /// <summary>Whether the gestures are off while a DSA app window is in front. On by
    /// default.</summary>
    public bool TouchGesturesOff => _settings?.TouchGesturesOff ?? true;

    /// <summary>What the window manager last said about the gestures.</summary>
    public TouchGesturesState TouchGesturesState => _settings?.TouchGesturesState ?? TouchGesturesState.Unknown;

    /// <summary>Whether the switch is shown: not on a Windows without such gestures.</summary>
    public bool ShowTouchGestures => TouchGesturesState switch
    {
        TouchGesturesState.Unsupported => false,
        TouchGesturesState.Unknown => WindowsBuild >= TouchGesturesBuild,
        _ => true,
    };

    /// <summary>Whether Windows refused, so the switch cannot be used.</summary>
    public bool TouchGesturesFailed => TouchGesturesState == TouchGesturesState.Failed;

    /// <summary>Whether the switch can be changed now.</summary>
    public bool CanChangeTouchGestures => CanChange && !TouchGesturesFailed;

    /// <summary>Why the switch cannot be used, in the user's language, with what Windows
    /// said.</summary>
    public string TouchGesturesProblemText => TouchGesturesFailed
        ? string.Format(CultureInfo.CurrentCulture, _text.Get("TouchGestures_Failed"),
            _settings?.TouchGesturesProblem ?? _text.Get("TouchGestures_NoReason"))
        : "";

    public Task SetTouchGesturesOffAsync(bool on) =>
        (on == TouchGesturesOff && Loaded) || TouchGesturesFailed
            ? Task.CompletedTask
            : ChangeAsync(() => _service.SetTouchGesturesOffAsync(on));
}
