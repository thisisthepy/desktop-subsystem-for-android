using System.Globalization;

namespace Dsa.Settings.Core;

/// <summary>An app that runs in the background as the page lists it: its package, name,
/// what its notification says (and what it plays), and its icon.</summary>
public sealed record BackgroundAppRow(string Package, string Label, string Detail, byte[]? Icon);

/// <summary>The "Running in the background" page: the apps in Android that keep running with
/// no window because they run a foreground service (music, a download, a VPN), each with a
/// button that opens it and one that stops it, and whether stopping one from the notification
/// area asks first. Reading the list never starts Android.</summary>
public sealed class BackgroundViewModel : ObservableObject
{
    private readonly DsaService _service;
    private readonly ITextSource _text;
    private BackgroundList? _list;
    private bool _askBeforeStop = true;
    private string? _error;
    private string? _message;
    private bool _busy;

    public BackgroundViewModel(DsaService service, ITextSource text)
    {
        _service = service;
        _text = text;
    }

    public bool IsBusy => _busy;
    public bool IsIdle => !_busy;
    public string Error => _error ?? "";
    public bool HasError => _error is not null;
    public string Message => _message ?? "";
    public bool HasMessage => _message is not null;
    public bool Loaded => _list is not null;
    public bool HasNone => _list is { Apps.Count: 0 };

    /// <summary>Lets go of the list and its app icons, for a page that is left: the page asks
    /// for the list afresh each time it is shown, so the old one is never seen again. A run in
    /// progress keeps what it brings.</summary>
    public void Release()
    {
        if (_busy)
        {
            return;
        }
        _list = null;
        _message = null;
        ChangedAll();
    }

    /// <summary>Whether stopping an app from the notification area asks first.</summary>
    public bool AskBeforeStop => _askBeforeStop;

    public IReadOnlyList<BackgroundAppRow> Apps => (_list?.Apps ?? Array.Empty<BackgroundApp>())
        .Select(a => new BackgroundAppRow(
            a.Package,
            a.Label.Length > 0 ? a.Label : a.Package,
            Detail(a),
            a.IconPng.Length > 0 ? a.IconPng : null))
        .ToList();

    /// <summary>The notification's title and text, and what the app plays when it has a media
    /// session.</summary>
    private string Detail(BackgroundApp app)
    {
        var parts = new List<string>();
        var said = string.Join(" · ", new[] { app.Title, app.Text }.Where(s => s.Trim().Length > 0));
        if (said.Length > 0)
        {
            parts.Add(said);
        }
        var media = _list?.Media.FirstOrDefault(m => m.Package == app.Package);
        if (media is not null && media.Title.Length > 0)
        {
            var key = media.Playback == "playing" ? "Background_Playing" : "Background_Paused";
            var what = media.Artist.Length > 0 ? media.Title + " - " + media.Artist : media.Title;
            parts.Add(string.Format(CultureInfo.CurrentCulture, _text.Get(key), what));
        }
        return parts.Count > 0 ? string.Join("\n", parts) : app.Package;
    }

    private async Task Run(Func<Task<string?>> work)
    {
        if (_busy)
        {
            return;
        }
        _busy = true;
        _message = null;
        ChangedAll();
        try
        {
            _message = await work().ConfigureAwait(true);
            _list = await _service.BackgroundAsync().ConfigureAwait(true);
            _error = null;
        }
        catch (Exception e)
        {
            _error = Format.Problem(_text, e);
        }
        finally
        {
            _busy = false;
        }
        ChangedAll();
    }

    /// <summary>Reads the apps that run in the background, and whether a stop asks first.</summary>
    public Task RefreshAsync() => Run(async () =>
    {
        var settings = await _service.SettingsAsync().ConfigureAwait(true);
        _askBeforeStop = !settings.StopWithoutAsking;
        return null;
    });

    /// <summary>Stops <paramref name="package"/>. The page's button is the user's own choice, so
    /// it does not ask again.</summary>
    public Task StopAsync(string package) => Run(async () =>
    {
        var label = _list?.Apps.FirstOrDefault(a => a.Package == package) is { } app && app.Label.Length > 0
            ? app.Label
            : package;
        await _service.StopBackgroundAsync(package).ConfigureAwait(true);
        return string.Format(CultureInfo.CurrentCulture, _text.Get("Background_Stopped"), label);
    });

    /// <summary>Opens the window of <paramref name="package"/>.</summary>
    public Task OpenAsync(string package) => Run(async () =>
    {
        await _service.OpenBackgroundAsync(package).ConfigureAwait(true);
        return null;
    });

    /// <summary>Whether stopping an app from the notification area asks first.</summary>
    public Task SetAskBeforeStopAsync(bool ask) => Run(async () =>
    {
        var settings = await _service.SetStopWithoutAskingAsync(!ask).ConfigureAwait(true);
        _askBeforeStop = !settings.StopWithoutAsking;
        return null;
    });
}
