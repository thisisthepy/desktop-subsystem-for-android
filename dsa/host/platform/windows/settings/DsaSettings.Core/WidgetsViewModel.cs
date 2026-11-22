using System.Globalization;

namespace Dsa.Settings.Core;

/// <summary>A change to the placed widgets, as <c>dsa widgets watch</c> reports it.</summary>
public abstract record WidgetChange
{
    /// <summary>The widgets placed now, all of them.</summary>
    public sealed record All(IReadOnlyList<PlacedWidget> Widgets) : WidgetChange;

    /// <summary>A widget was placed, or described again.</summary>
    public sealed record Placed(PlacedWidget Widget) : WidgetChange;

    /// <summary>A widget was removed, from this page, from its window's menu or by dsa.</summary>
    public sealed record Removed(int Id) : WidgetChange;
}

/// <summary>One widget on offer as a card in its app's grid: its name, its default size and
/// whether it asks to be set up, and its preview. Until Android has drawn the preview, and
/// when the widget has none, the card shows its app's icon; null when there is neither.</summary>
public sealed class WidgetCard : ObservableObject
{
    private readonly byte[]? _appIcon;
    private byte[]? _drawn;

    public WidgetCard(string provider, string label, string app, string size, string detail, byte[]? appIcon)
    {
        Provider = provider;
        Label = label;
        App = app;
        Size = size;
        Detail = detail;
        _appIcon = appIcon;
    }

    public string Provider { get; }
    public string Label { get; }

    /// <summary>The name of the app that offers the widget.</summary>
    public string App { get; }

    /// <summary>The widget's default size in dp, as the card shows it.</summary>
    public string Size { get; }

    /// <summary>The size, and whether the widget asks to be set up, as a screen reader reads
    /// it after the widget's name.</summary>
    public string Detail { get; }

    public byte[]? Preview => _drawn ?? _appIcon;
    public bool NoPreview => Preview is null;

    /// <summary>Whether the card shows a preview Android drew, rather than the app's icon.</summary>
    public bool HasDrawnPreview => _drawn is not null;

    internal void Show(byte[]? drawn)
    {
        if (ReferenceEquals(drawn, _drawn))
        {
            return;
        }
        _drawn = drawn;
        Changed(nameof(Preview));
        Changed(nameof(NoPreview));
        Changed(nameof(HasDrawnPreview));
    }
}

/// <summary>One app that offers widgets, as the Widgets page lists it: one entry, closed until
/// the user opens it, with the app's icon, name and how many widgets it has. Open, it shows
/// its widgets as a grid of cards, a page at a time.</summary>
public sealed class WidgetAppRow : ObservableObject
{
    private readonly ITextSource _text;
    private readonly IReadOnlyList<WidgetCard> _all;
    private bool _expanded;
    private int _shown;
    private IReadOnlyList<WidgetCard> _cards = Array.Empty<WidgetCard>();

    public WidgetAppRow(string package, string name, byte[]? icon, IReadOnlyList<WidgetCard> cards, ITextSource text,
        bool expanded, int shown)
    {
        Package = package;
        Name = name;
        Icon = icon;
        _all = cards;
        _text = text;
        Apply(expanded, shown);
    }

    private void Apply(bool expanded, int shown)
    {
        _expanded = expanded;
        _shown = Math.Clamp(shown, 0, _all.Count);
        // One list for as long as the page shows the same cards, so the grid is not drawn anew
        // each time something else about the app changes.
        _cards = _expanded ? _all.Take(_shown).ToList() : Array.Empty<WidgetCard>();
    }

    public string Package { get; }
    public string Name { get; }
    public byte[]? Icon { get; }
    public bool NoIcon => Icon is null;
    public int Count => _all.Count;

    /// <summary>How many widgets the app offers, in words.</summary>
    public string CountText => _all.Count == 1
        ? _text.Get("Widgets_CountOne")
        : string.Format(CultureInfo.CurrentCulture, _text.Get("Widgets_Count"), _all.Count);

    public bool IsExpanded => _expanded;

    /// <summary>Every widget the app offers, shown or not.</summary>
    public IReadOnlyList<WidgetCard> AllCards => _all;

    /// <summary>The widgets the open app shows now: the first pages of them. None while closed.</summary>
    public IReadOnlyList<WidgetCard> Cards => _cards;

    public bool HasMore => _expanded && _shown < _all.Count;

    /// <summary>What the button that shows the next page says: how many it adds.</summary>
    public string MoreText => string.Format(CultureInfo.CurrentCulture, _text.Get("Widgets_ShowMore"),
        Math.Min(WidgetsViewModel.PageSize, _all.Count - _shown));

    internal int Shown => _shown;

    internal void Set(bool expanded, int shown)
    {
        Apply(expanded, shown);
        ChangedAll();
    }
}

/// <summary>A placed widget as the Widgets page lists it.</summary>
public sealed record PlacedWidgetRow(int Id, string Label, string App, string Detail);

/// <summary>The previews Android drew, kept for a while so an app opened again shows them at
/// once. Bounded by count and by bytes: the least recently used go first, so the page never
/// holds every widget's image of an app with hundreds of them.</summary>
public sealed class WidgetPreviewCache
{
    private readonly int _maxEntries;
    private readonly long _maxBytes;
    private readonly LinkedList<(string Provider, byte[] Png)> _order = new();
    private readonly Dictionary<string, LinkedListNode<(string Provider, byte[] Png)>> _byProvider = new(StringComparer.Ordinal);
    private long _bytes;

    public WidgetPreviewCache(int maxEntries = 160, long maxBytes = 4 * 1024 * 1024)
    {
        _maxEntries = maxEntries;
        _maxBytes = maxBytes;
    }

    public int Count => _byProvider.Count;
    public long Bytes => _bytes;

    /// <summary>The preview kept for <paramref name="provider"/>, which becomes the most
    /// recently used; null when none is kept.</summary>
    public byte[]? Get(string provider)
    {
        if (!_byProvider.TryGetValue(provider, out var node))
        {
            return null;
        }
        _order.Remove(node);
        _order.AddFirst(node);
        return node.Value.Png;
    }

    public void Put(string provider, byte[] png)
    {
        if (_byProvider.TryGetValue(provider, out var old))
        {
            _order.Remove(old);
            _byProvider.Remove(provider);
            _bytes -= old.Value.Png.Length;
        }
        if (png.Length == 0 || png.Length > _maxBytes)
        {
            return;
        }
        _byProvider[provider] = _order.AddFirst((provider, png));
        _bytes += png.Length;
        while (_byProvider.Count > _maxEntries || _bytes > _maxBytes)
        {
            var last = _order.Last!;
            _order.RemoveLast();
            _byProvider.Remove(last.Value.Provider);
            _bytes -= last.Value.Png.Length;
        }
    }

    public void Clear()
    {
        _order.Clear();
        _byProvider.Clear();
        _bytes = 0;
    }
}

/// <summary>The Widgets page: the widgets the apps in Android offer, which the user places on
/// the desktop, and the ones placed, which the user removes. The widgets on offer are listed
/// by the app that offers them, one entry per app, each opened and closed on its own; an open
/// app shows its widgets as a grid of cards, a page at a time, and only then asks Android to
/// draw their previews, so an app with hundreds of widgets costs nothing until it is opened.
/// Listing them starts Android when it is not running. While the page watches, a widget
/// placed or removed anywhere (its window's menu, dsa) shows here at once.</summary>
public sealed class WidgetsViewModel : ObservableObject
{
    private static readonly string[] WatchArgs = { "widgets", "watch", "--json" };

    /// <summary>How many widget cards an open app shows at first, and how many more each
    /// "show more" adds.</summary>
    public const int PageSize = 24;

    /// <summary>How many previews one request to dsa asks for.</summary>
    internal const int PreviewBatch = 32;

    private readonly DsaService _service;
    private readonly ITextSource _text;
    private readonly ILogStream? _stream;
    private readonly HashSet<string> _expanded = new(StringComparer.Ordinal);
    private readonly Dictionary<string, int> _shown = new(StringComparer.Ordinal);
    private readonly WidgetPreviewCache _previews = new();

    /// <summary>The widgets whose previews are being asked for now, so none is asked twice at
    /// once.</summary>
    private readonly HashSet<string> _asking = new(StringComparer.Ordinal);

    /// <summary>The widgets Android drew no preview for (or that are no longer on offer), not
    /// asked for again until the page lets go of its previews.</summary>
    private readonly HashSet<string> _undrawn = new(StringComparer.Ordinal);

    private IReadOnlyList<WidgetAppRow> _apps = Array.Empty<WidgetAppRow>();
    private IReadOnlyList<PlacedWidget>? _placed;
    private string? _error;
    private string? _message;
    private bool _busy;

    public WidgetsViewModel(DsaService service, ITextSource text, ILogStream? stream = null)
    {
        _service = service;
        _text = text;
        _stream = stream;
    }

    public bool IsBusy => _busy;
    public bool IsIdle => !_busy;
    public string Error => _error ?? "";
    public bool HasError => _error is not null;
    public string Message => _message ?? "";
    public bool HasMessage => _message is not null;
    public bool Loaded => _placed is not null;
    public bool HasNoOffers => _placed is not null && _apps.Count == 0;
    public bool HasNoPlaced => _placed is { Count: 0 };

    /// <summary>The apps that offer widgets, in name order, each with its widgets.</summary>
    public IReadOnlyList<WidgetAppRow> Apps => _apps;

    /// <summary>The previews kept now, for the checks of its bounds.</summary>
    public WidgetPreviewCache PreviewCache => _previews;

    private static string AppName(string appLabel, string package) => appLabel.Length > 0 ? appLabel : package;

    public IReadOnlyList<PlacedWidgetRow> Placed => (_placed ?? Array.Empty<PlacedWidget>())
        .Select(w => new PlacedWidgetRow(
            w.Id,
            w.Label.Length > 0 ? w.Label : w.Provider,
            AppName(w.AppLabel, PackageOf(w.Provider)),
            string.Format(CultureInfo.CurrentCulture, _text.Get("Widgets_At"), w.X, w.Y, w.WidthDp, w.HeightDp)))
        .ToList();

    private static string PackageOf(string provider)
    {
        var slash = provider.IndexOf('/');
        return slash > 0 ? provider[..slash] : provider;
    }

    private IReadOnlyList<WidgetAppRow> Rows(WidgetGallery gallery) => gallery.Apps
        .Select(app =>
        {
            var name = AppName(app.AppLabel, app.Package);
            var icon = app.IconPng.Length > 0 ? app.IconPng : null;
            var cards = app.Widgets
                .Select(o =>
                {
                    var size = string.Format(CultureInfo.CurrentCulture, _text.Get("Widgets_Size"),
                        o.DefaultWidthDp, o.DefaultHeightDp);
                    var card = new WidgetCard(
                        o.Provider,
                        o.Label.Length > 0 ? o.Label : o.Provider,
                        name,
                        size,
                        size + (o.NeedsConfiguration ? " · " + _text.Get("Widgets_Setup") : ""),
                        icon);
                    return card;
                })
                .ToList();
            var expanded = _expanded.Contains(app.Package);
            var row = new WidgetAppRow(app.Package, name, icon, cards, _text, expanded,
                _shown.TryGetValue(app.Package, out var shown) ? shown : PageSize);
            if (expanded)
            {
                foreach (var card in row.Cards)
                {
                    card.Show(_previews.Get(card.Provider));
                }
            }
            return row;
        })
        .ToList();

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
            var gallery = await _service.WidgetGalleryAsync().ConfigureAwait(true);
            _placed = gallery.Placed;
            _apps = Rows(gallery);
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
        await LoadPreviewsAsync().ConfigureAwait(true);
    }

    /// <summary>Reads the widgets on offer and the ones placed, and the previews of the open
    /// apps' widgets.</summary>
    public Task RefreshAsync() => Run(() => Task.FromResult<string?>(null));

    /// <summary>Opens or closes the app <paramref name="package"/>'s widgets. An app opened
    /// shows its first page of cards and asks for their previews; one closed lets go of its
    /// cards' images (the previews stay in the bounded cache) and starts from its first page
    /// when opened again.</summary>
    public Task SetExpandedAsync(string package, bool expanded)
    {
        var row = _apps.FirstOrDefault(a => a.Package == package);
        if (expanded)
        {
            _expanded.Add(package);
        }
        else
        {
            _expanded.Remove(package);
            _shown.Remove(package);
        }
        if (row is null)
        {
            return Task.CompletedTask;
        }
        if (!expanded)
        {
            foreach (var card in row.AllCards)
            {
                card.Show(null);
            }
            row.Set(false, PageSize);
            return Task.CompletedTask;
        }
        row.Set(true, Math.Max(row.Shown, PageSize));
        foreach (var card in row.Cards)
        {
            card.Show(_previews.Get(card.Provider));
        }
        return LoadPreviewsAsync();
    }

    /// <summary>Shows the next page of the open app <paramref name="package"/>'s widgets and
    /// asks for their previews.</summary>
    public Task ShowMoreAsync(string package)
    {
        var row = _apps.FirstOrDefault(a => a.Package == package);
        if (row is null || !row.IsExpanded || !row.HasMore)
        {
            return Task.CompletedTask;
        }
        var shown = row.Shown + PageSize;
        _shown[package] = shown;
        row.Set(true, shown);
        foreach (var card in row.Cards)
        {
            card.Show(_previews.Get(card.Provider));
        }
        return LoadPreviewsAsync();
    }

    /// <summary>Asks Android for the previews of the widgets shown that have none yet, a batch
    /// at a time. A widget whose preview did not come shows its app's icon. When dsa fails,
    /// the cards keep their icons and the page asks again the next time it is read.</summary>
    private async Task LoadPreviewsAsync()
    {
        while (true)
        {
            var wanted = _apps
                .Where(a => a.IsExpanded)
                .SelectMany(a => a.Cards)
                .Where(c => !c.HasDrawnPreview && !_asking.Contains(c.Provider) && !_undrawn.Contains(c.Provider))
                .Select(c => c.Provider)
                .Distinct(StringComparer.Ordinal)
                .Take(PreviewBatch)
                .ToList();
            if (wanted.Count == 0)
            {
                return;
            }
            _asking.UnionWith(wanted);
            IReadOnlyList<WidgetPreview> previews;
            try
            {
                previews = await _service.WidgetPreviewsAsync(wanted).ConfigureAwait(true);
            }
            catch (DsaException)
            {
                return;
            }
            finally
            {
                _asking.ExceptWith(wanted);
            }
            var drawn = new Dictionary<string, byte[]>(StringComparer.Ordinal);
            foreach (var preview in previews.Where(p => p.Png.Length > 0))
            {
                drawn[preview.Provider] = preview.Png;
                _previews.Put(preview.Provider, preview.Png);
            }
            _undrawn.UnionWith(wanted.Where(p => !drawn.ContainsKey(p)));
            foreach (var card in _apps.Where(a => a.IsExpanded).SelectMany(a => a.Cards))
            {
                if (!card.HasDrawnPreview && drawn.TryGetValue(card.Provider, out var png))
                {
                    card.Show(png);
                }
            }
        }
    }

    /// <summary>Lets go of every preview, for a page that is left: an open app's images would
    /// stay in memory for nothing. Which apps are open is kept, and their previews are asked
    /// for again when the page is shown.</summary>
    public void ReleasePreviews()
    {
        _previews.Clear();
        _undrawn.Clear();
        foreach (var card in _apps.SelectMany(a => a.AllCards))
        {
            card.Show(null);
        }
    }

    /// <summary>Places a widget of <paramref name="provider"/> on the desktop.</summary>
    public Task AddAsync(string provider) => Run(async () =>
    {
        var placed = await _service.AddWidgetAsync(provider).ConfigureAwait(true);
        return string.Format(CultureInfo.CurrentCulture, _text.Get("Widgets_Placed"),
            placed.Label.Length > 0 ? placed.Label : placed.Provider);
    });

    /// <summary>Removes the widget <paramref name="id"/> from the desktop.</summary>
    public Task RemoveAsync(int id) => Run(async () =>
    {
        var label = _placed?.FirstOrDefault(w => w.Id == id)?.Label ?? id.ToString(CultureInfo.CurrentCulture);
        await _service.RemoveWidgetAsync(id).ConfigureAwait(true);
        return string.Format(CultureInfo.CurrentCulture, _text.Get("Widgets_Removed"), label);
    });

    /// <summary>Applies a change that came while watching to the placed widgets shown. The
    /// widgets on offer stay as they were read.</summary>
    public void Apply(WidgetChange change)
    {
        var placed = (_placed ?? Array.Empty<PlacedWidget>()).ToList();
        switch (change)
        {
            case WidgetChange.All all:
                placed = all.Widgets.ToList();
                break;
            case WidgetChange.Placed one:
                var at = placed.FindIndex(w => w.Id == one.Widget.Id);
                if (at >= 0)
                {
                    placed[at] = one.Widget;
                }
                else
                {
                    placed.Add(one.Widget);
                    placed.Sort((a, b) => a.Id.CompareTo(b.Id));
                }
                break;
            case WidgetChange.Removed gone:
                placed.RemoveAll(w => w.Id == gone.Id);
                break;
        }
        _placed = placed;
        Changed(nameof(Placed));
        Changed(nameof(HasNoPlaced));
        Changed(nameof(Loaded));
        Changed(nameof(HasNoOffers));
    }

    /// <summary>Watches the placed widgets once, through <c>dsa widgets watch</c>, until it
    /// ends or <paramref name="cancel"/> stops it. Does nothing without a stream.</summary>
    public async Task WatchOnceAsync(CancellationToken cancel)
    {
        if (_stream is null)
        {
            return;
        }
        await foreach (var line in _stream.LinesAsync(WatchArgs, cancel).ConfigureAwait(true))
        {
            if (DsaService.ParseWidgetChange(line) is { } change)
            {
                Apply(change);
            }
        }
    }

    /// <summary>Watches the placed widgets until <paramref name="cancel"/> stops it, watching
    /// again after <paramref name="retry"/> whenever the watch ends, as it does when Android
    /// stops.</summary>
    public async Task WatchAsync(TimeSpan retry, CancellationToken cancel)
    {
        while (!cancel.IsCancellationRequested)
        {
            try
            {
                await WatchOnceAsync(cancel).ConfigureAwait(true);
            }
            catch (OperationCanceledException)
            {
                return;
            }
            catch (DsaException)
            {
                // Android stopped or the service is away: the page keeps what it shows, and the
                // watch starts again below.
            }
            try
            {
                await Task.Delay(retry, cancel).ConfigureAwait(true);
            }
            catch (OperationCanceledException)
            {
                return;
            }
        }
    }
}
