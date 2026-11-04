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

/// <summary>A widget on offer as the Widgets page lists it: its name, the app that offers it,
/// its size and whether it asks to be set up, and its preview (else the app's icon), null when
/// it has neither.</summary>
public sealed record WidgetOfferRow(string Provider, string Label, string App, string Detail, byte[]? Preview)
{
    /// <summary>Whether the page shows a stand-in icon, as the widget has no picture.</summary>
    public bool NoPreview => Preview is null;
}

/// <summary>A placed widget as the Widgets page lists it.</summary>
public sealed record PlacedWidgetRow(int Id, string Label, string App, string Detail);

/// <summary>The Widgets page: the widgets the apps in Android offer, which the user places on
/// the desktop, and the ones placed, which the user removes. Listing them starts Android when
/// it is not running. While the page watches, a widget placed or removed anywhere (its
/// window's menu, dsa) shows here at once.</summary>
public sealed class WidgetsViewModel : ObservableObject
{
    private static readonly string[] WatchArgs = { "widgets", "watch", "--json" };

    private readonly DsaService _service;
    private readonly ITextSource _text;
    private readonly ILogStream? _stream;
    private WidgetList? _list;
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
    public bool Loaded => _list is not null;
    public bool HasNoOffers => _list is { Offers.Count: 0 };
    public bool HasNoPlaced => _list is { Placed.Count: 0 };

    private static string AppName(string appLabel, string package) => appLabel.Length > 0 ? appLabel : package;

    public IReadOnlyList<WidgetOfferRow> Offers => (_list?.Offers ?? Array.Empty<WidgetOffer>())
        .Select(o => new WidgetOfferRow(
            o.Provider,
            o.Label.Length > 0 ? o.Label : o.Provider,
            AppName(o.AppLabel, o.Package),
            string.Format(CultureInfo.CurrentCulture, _text.Get("Widgets_Size"), o.DefaultWidthDp, o.DefaultHeightDp)
                + (o.NeedsConfiguration ? " · " + _text.Get("Widgets_Setup") : ""),
            o.PreviewPng.Length > 0 ? o.PreviewPng : null))
        .ToList();

    public IReadOnlyList<PlacedWidgetRow> Placed => (_list?.Placed ?? Array.Empty<PlacedWidget>())
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
            _list = await _service.WidgetsAsync().ConfigureAwait(true);
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

    /// <summary>Reads the widgets on offer and the ones placed.</summary>
    public Task RefreshAsync() => Run(() => Task.FromResult<string?>(null));

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
        var label = _list?.Placed.FirstOrDefault(w => w.Id == id)?.Label ?? id.ToString(CultureInfo.CurrentCulture);
        await _service.RemoveWidgetAsync(id).ConfigureAwait(true);
        return string.Format(CultureInfo.CurrentCulture, _text.Get("Widgets_Removed"), label);
    });

    /// <summary>Applies a change that came while watching to the placed widgets shown. The
    /// widgets on offer stay as they were read.</summary>
    public void Apply(WidgetChange change)
    {
        var offers = _list?.Offers ?? Array.Empty<WidgetOffer>();
        var placed = (_list?.Placed ?? Array.Empty<PlacedWidget>()).ToList();
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
        _list = new WidgetList(offers, placed);
        ChangedAll();
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
