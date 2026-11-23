using System.Globalization;

namespace Dsa.Settings.Core;

/// <summary>An app as the Games page lists it: its package, name, a line that says whether it
/// is a game, whether it keeps its focus, and its icon.</summary>
public sealed record GameAppRow(string Package, string Label, string Detail, bool Game, bool KeepFocus, byte[]? Icon);

/// <summary>The Games page. Its "Game convenience" section holds "Keep game focus": every
/// installed app with a switch, games first, off for every app until the user turns it on. An
/// app with it on stays focused inside Android while the user works in another window, so a
/// game that pauses when it loses the focus keeps running; keys still go only to the window
/// the user is in. "Select all" turns every app on, and pressed again (every app on) turns
/// every app off. Reading the list never starts Android.</summary>
public sealed class GamesViewModel : ObservableObject
{
    private readonly DsaService _service;
    private readonly ITextSource _text;
    private IReadOnlyList<GameApp>? _apps;
    private string? _error;
    private bool _busy;

    public GamesViewModel(DsaService service, ITextSource text)
    {
        _service = service;
        _text = text;
    }

    public bool IsBusy => _busy;
    public bool IsIdle => !_busy;
    public string Error => _error ?? "";
    public bool HasError => _error is not null;
    public bool Loaded => _apps is not null;
    public bool HasNone => _apps is { Count: 0 };
    public bool HasApps => _apps is { Count: > 0 };

    /// <summary>Whether every app keeps its focus, so Select all turns every app off.</summary>
    public bool AllOn => _apps is { Count: > 0 } apps && apps.All(a => a.KeepFocus);

    public IReadOnlyList<GameAppRow> Apps => (_apps ?? Array.Empty<GameApp>())
        .Select(a => new GameAppRow(
            a.Package,
            a.Label.Length > 0 ? a.Label : a.Package,
            a.Game
                ? string.Format(CultureInfo.CurrentCulture, _text.Get("Games_GameDetail"), a.Package)
                : a.Package,
            a.Game,
            a.KeepFocus,
            a.IconPng.Length > 0 ? a.IconPng : null))
        .ToList();

    /// <summary>Lets go of the list and its icons, for a page that is left: the page reads the
    /// list afresh each time it is shown. A run in progress keeps what it brings.</summary>
    public void Release()
    {
        if (_busy)
        {
            return;
        }
        _apps = null;
        ChangedAll();
    }

    private async Task Run(Func<Task<IReadOnlyList<GameApp>>> work)
    {
        if (_busy)
        {
            return;
        }
        _busy = true;
        ChangedAll();
        try
        {
            _apps = await work().ConfigureAwait(true);
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

    /// <summary>Reads the installed apps and their switches.</summary>
    public Task RefreshAsync() => Run(_service.GamesAsync);

    /// <summary>Turns keep game focus on or off for <paramref name="package"/>.</summary>
    public Task SetKeepFocusAsync(string package, bool on) => Run(() => _service.SetKeepFocusAsync(package, on));

    /// <summary>Turns every app on, or every app off when every one is on already.</summary>
    public Task SelectAllAsync() => Run(_service.SelectAllGamesAsync);
}
