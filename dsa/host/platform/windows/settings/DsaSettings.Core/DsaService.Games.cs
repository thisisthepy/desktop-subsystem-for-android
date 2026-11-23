using System.Text.Json;

namespace Dsa.Settings.Core;

/// <summary>One app of the Games page, as <c>dsa games</c> lists it: whether Android calls it a
/// game, and whether it keeps its focus while its window is not the active one.</summary>
public sealed record GameApp(string Package, string Label, bool Game, bool KeepFocus, byte[] IconPng);

/// <summary>The Games page's list: <c>dsa games</c>, which reads the apps the service keeps and
/// never starts Android. The order is the service's: games first.</summary>
public sealed partial class DsaService
{
    /// <summary>Reads what <c>dsa games ... --json</c> prints: <c>apps</c>, each with its
    /// <c>package</c>, <c>label</c>, <c>game</c>, <c>keep_focus</c> and <c>icon_png</c>.</summary>
    public static IReadOnlyList<GameApp> ReadGames(JsonElement json) => Items(json, "apps")
        .Where(a => a.ValueKind == JsonValueKind.Object)
        .Select(a => new GameApp(String(a, "package"), String(a, "label"), Bool(a, "game"),
            Bool(a, "keep_focus"), Base64(a, "icon_png")))
        .Where(a => a.Package.Length > 0)
        .ToList();

    /// <summary>The installed apps, games first, with whether each keeps its focus.</summary>
    public async Task<IReadOnlyList<GameApp>> GamesAsync() =>
        ReadGames(await RunJsonAsync("games", "list").ConfigureAwait(true));

    /// <summary>Turns keep game focus on or off for <paramref name="package"/>.</summary>
    public async Task<IReadOnlyList<GameApp>> SetKeepFocusAsync(string package, bool on) =>
        ReadGames(await RunJsonAsync("games", "keep-focus", on ? "on" : "off", package).ConfigureAwait(true));

    /// <summary>Select all: turns keep game focus on for every app, or off for every app when
    /// every one is on already.</summary>
    public async Task<IReadOnlyList<GameApp>> SelectAllGamesAsync() =>
        ReadGames(await RunJsonAsync("games", "select-all").ConfigureAwait(true));
}
