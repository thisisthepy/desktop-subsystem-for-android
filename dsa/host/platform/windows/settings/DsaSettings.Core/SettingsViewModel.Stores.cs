using System.Globalization;

namespace Dsa.Settings.Core;

/// <summary>One row of the alternative stores section: the store's plain name, one line on
/// where it stands, and which of its buttons work.</summary>
public sealed record AltStoreRow(
    string Id,
    string Name,
    string Package,
    string Status,
    bool CanInstall,
    bool CanOpen,
    bool IsInstalling,
    bool IsInstalled);

/// <summary>The alternative stores section of the Advanced page, under Google Play: a button
/// for each store that installs it, or opens it once it is installed. A store is removed from
/// the app list like any other app.</summary>
public sealed partial class SettingsViewModel
{
    private IReadOnlyList<AltStoreInfo> _stores = DsaService.DefaultStores;
    private string? _installingStore;
    private readonly Dictionary<string, string> _storeErrors = new();

    /// <summary>The rows, in the order the service lists the stores.</summary>
    public IReadOnlyList<AltStoreRow> AltStores => _stores.Select(Row).ToList();

    private AltStoreRow Row(AltStoreInfo store)
    {
        var installing = _installingStore == store.Id;
        string status;
        if (installing)
        {
            status = _text.Get("AltStore_Installing");
        }
        else if (_storeErrors.TryGetValue(store.Id, out var error))
        {
            status = string.Format(CultureInfo.CurrentCulture, _text.Get("AltStore_Failed"), error);
        }
        else if (store.Installed)
        {
            status = store.Version.Length > 0
                ? string.Format(CultureInfo.CurrentCulture, _text.Get("AltStore_InstalledVersion"), store.Version)
                : _text.Get("AltStore_Installed");
        }
        else
        {
            status = _text.Get("AltStore_NotInstalled");
        }
        return new AltStoreRow(store.Id, store.Name, store.Package, status,
            CanInstall: !_busy && !store.Installed,
            CanOpen: !_busy && store.Installed,
            IsInstalling: installing,
            IsInstalled: store.Installed);
    }

    /// <summary>Reads where the stores stand. A service that cannot say leaves every store
    /// offered for install.</summary>
    private async Task LoadStoresAsync()
    {
        try
        {
            _stores = await _service.StoresAsync().ConfigureAwait(true);
        }
        catch (DsaException)
        {
            _stores = DsaService.DefaultStores;
        }
        ChangedAll();
    }

    public async Task InstallStoreAsync(string id)
    {
        if (_busy || _stores.All(s => s.Id != id))
        {
            return;
        }
        _busy = true;
        _installingStore = id;
        _storeErrors.Remove(id);
        ChangedAll();
        try
        {
            _stores = await _service.InstallStoreAsync(id).ConfigureAwait(true);
        }
        catch (Exception e)
        {
            // The row says why, and the store stays offered for install.
            _storeErrors[id] = Format.Problem(_text, e);
        }
        finally
        {
            _busy = false;
            _installingStore = null;
            ChangedAll();
        }
    }

    public async Task OpenStoreAsync(string id)
    {
        if (_stores.FirstOrDefault(s => s.Id == id) is not { Installed: true } store)
        {
            return;
        }
        _storeErrors.Remove(id);
        try
        {
            await _service.OpenStoreAsync(store.Package).ConfigureAwait(true);
        }
        catch (Exception e)
        {
            _storeErrors[id] = Format.Problem(_text, e);
        }
        ChangedAll();
    }
}
