using System.Globalization;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>The alternative stores section under Google Play: a row for each store that says
/// whether it is installed, installs it through <c>dsa stores install</c>, opens it once it is
/// installed, and says why an install failed.</summary>
public class AltStoresViewModelTests
{
    private const string Settings =
        "{\"render_gpu\":\"display\",\"freeze\":false,\"program_updates\":true,\"image_updates\":true," +
        "\"program_updates_available\":true,\"image_updates_available\":false,\"version\":\"0.0.9\"}";

    private static string Stores(bool auroraInstalled) =>
        "{\"stores\":[" +
        "{\"id\":\"onestore\",\"name\":\"ONE store\",\"package\":\"com.skt.skaf.A000Z00040\",\"homepage\":\"https://m.onestore.co.kr/\",\"installed\":false,\"version\":\"\"}," +
        "{\"id\":\"aurora\",\"name\":\"Aurora Store\",\"package\":\"com.aurora.store\",\"homepage\":\"https://auroraoss.com/\",\"installed\":" +
        (auroraInstalled ? "true" : "false") + ",\"version\":\"" + (auroraInstalled ? "4.8.4" : "") + "\"}," +
        "{\"id\":\"fdroid\",\"name\":\"F-Droid\",\"package\":\"org.fdroid.fdroid\",\"homepage\":\"https://f-droid.org/\",\"installed\":false,\"version\":\"\"}]}";

    public AltStoresViewModelTests()
    {
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
    }

    private static FakeCli Cli() => new FakeCli()
        .Answer("settings --json", Settings)
        .Fail("gpus --json", "dsa: no GPU list")
        .Answer("stores status --json", Stores(false));

    [Fact]
    public async Task Fr43_each_store_has_a_row_that_offers_install_until_it_is_installed()
    {
        var cli = Cli().Answer("stores install aurora --json", Stores(true));
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.Equal(new[] { "ONE store", "Aurora Store", "F-Droid" }, vm.AltStores.Select(r => r.Name));
        Assert.All(vm.AltStores, r =>
        {
            Assert.Equal("AltStore_NotInstalled", r.Status);
            Assert.True(r.CanInstall);
            Assert.False(r.CanOpen);
        });

        await vm.InstallStoreAsync("aurora");
        Assert.Contains("stores install aurora --json", cli.Calls);
        var aurora = vm.AltStores[1];
        Assert.Equal("AltStore_InstalledVersion", aurora.Status);
        Assert.True(aurora.IsInstalled);
        Assert.False(aurora.CanInstall);
        Assert.True(aurora.CanOpen);
        Assert.False(aurora.IsInstalling);

        await vm.OpenStoreAsync("aurora");
        Assert.Equal("launch com.aurora.store", cli.Calls[^1]);
    }

    [Fact]
    public async Task Fr43_a_refused_install_says_why_and_keeps_install_offered()
    {
        var cli = Cli().Fail("stores install fdroid --json",
            "dsa: the file downloaded for F-Droid is signed by a certificate with SHA-256 00, so it was not installed");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        await vm.InstallStoreAsync("fdroid");
        var fdroid = vm.AltStores[2];
        Assert.Equal("AltStore_Failed", fdroid.Status);
        Assert.True(fdroid.CanInstall);
        Assert.False(fdroid.IsInstalled);
    }

    [Fact]
    public async Task Fr43_a_service_that_cannot_list_the_stores_still_offers_all_three()
    {
        var cli = new FakeCli().Answer("settings --json", Settings).Fail("gpus --json", "dsa: no GPU list");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.Equal(new[] { "onestore", "aurora", "fdroid" }, vm.AltStores.Select(r => r.Id));
    }

    private static readonly string[] Keys =
    {
        "AltStoresHeader.Text", "AltStoresCard.Description", "AltStore_NotInstalled", "AltStore_Installed",
        "AltStore_InstalledVersion", "AltStore_Installing", "AltStore_Failed", "AltStoreInstallButton.Content",
        "AltStoreOpenButton.Content",
    };

    [Theory]
    [MemberData(nameof(StringTables.Files), MemberType = typeof(StringTables))]
    public void Fr43_alternative_store_strings_exist_in_every_language(string file)
    {
        var table = StringTables.Load(file);
        foreach (var key in Keys)
        {
            Assert.True(table.TryGetValue(key, out var text) && text.Length > 0, $"{file} has no {key}");
        }
        Assert.Equal(new[] { 0 }, StringTables.Holes(table["AltStore_InstalledVersion"]));
        Assert.Equal(new[] { 0 }, StringTables.Holes(table["AltStore_Failed"]));
    }
}
