using System.Text.Json;

namespace Dsa.Settings.Core;

/// <summary>The alternative stores: ONE store, Aurora Store and F-Droid, which <c>dsa stores</c>
/// downloads from each project's official site, checks and installs on the user's request.</summary>
public sealed partial class DsaService
{
    /// <summary>The stores, as a service from before <c>dsa stores</c> would have listed them, so
    /// the section still shows its buttons when the service cannot say where they stand.</summary>
    public static readonly IReadOnlyList<AltStoreInfo> DefaultStores = new[]
    {
        new AltStoreInfo("onestore", "ONE store", "com.skt.skaf.A000Z00040", "https://m.onestore.co.kr/", false, ""),
        new AltStoreInfo("aurora", "Aurora Store", "com.aurora.store", "https://auroraoss.com/", false, ""),
        new AltStoreInfo("fdroid", "F-Droid", "org.fdroid.fdroid", "https://f-droid.org/", false, ""),
    };

    /// <summary>Reads what <c>dsa stores ... --json</c> prints: <c>stores</c>, each with its
    /// <c>id</c>, <c>name</c>, <c>package</c>, <c>homepage</c>, <c>installed</c> and
    /// <c>version</c>.</summary>
    public static IReadOnlyList<AltStoreInfo> ReadStores(JsonElement json)
    {
        if (!json.TryGetProperty("stores", out var stores) || stores.ValueKind != JsonValueKind.Array)
        {
            return DefaultStores;
        }
        return stores.EnumerateArray()
            .Where(s => s.ValueKind == JsonValueKind.Object)
            .Select(s => new AltStoreInfo(String(s, "id"), String(s, "name"), String(s, "package"),
                WebAddress(s, "homepage", ""), Bool(s, "installed"), String(s, "version")))
            .Where(s => s.Id.Length > 0)
            .ToList();
    }

    /// <summary>Whether each store is installed in the user's Android.</summary>
    public async Task<IReadOnlyList<AltStoreInfo>> StoresAsync() =>
        ReadStores(await RunJsonAsync("stores", "status").ConfigureAwait(true));

    /// <summary>Downloads the store's newest APK from its official site, checks its package and
    /// signer, and installs it, starting Android first when it is not running.</summary>
    public async Task<IReadOnlyList<AltStoreInfo>> InstallStoreAsync(string id) =>
        ReadStores(await RunJsonAsync("stores", "install", id).ConfigureAwait(true));

    /// <summary>Opens the installed store in a window of its own.</summary>
    public async Task OpenStoreAsync(string package)
    {
        var result = await _cli.RunAsync(new[] { "launch", package }).ConfigureAwait(true);
        if (result.ExitCode != 0)
        {
            var detail = result.Error.StartsWith("dsa: ", StringComparison.Ordinal)
                ? result.Error["dsa: ".Length..]
                : result.Error;
            throw new DsaException(Problem.Refused, detail.Length > 0 ? detail : $"dsa ended with code {result.ExitCode}");
        }
    }
}

/// <summary>One alternative store, as <c>dsa stores</c> describes it.</summary>
public sealed record AltStoreInfo(
    string Id,
    string Name,
    string Package,
    string Homepage,
    bool Installed,
    string Version);
