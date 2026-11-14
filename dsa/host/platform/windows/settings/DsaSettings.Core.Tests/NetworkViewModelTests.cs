using System.Globalization;
using System.Xml.Linq;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>The Network page: Android's address, DNS servers and mode, and the advanced
/// networking switch.</summary>
public class NetworkViewModelTests
{
    private const string Nat =
        "{\"mode\":\"nat\",\"addresses\":[\"172.28.16.2\",\"fd5a:d5a:0:1::2\"]," +
        "\"dns_servers\":[\"172.28.16.1\"],\"gateway\":\"172.28.16.1\",\"problem\":null}";

    private static string Settings(bool advanced, string network) =>
        "{\"render_gpu\":\"display\",\"freeze\":false,\"program_updates\":true,\"image_updates\":true," +
        "\"program_updates_available\":true,\"image_updates_available\":false,\"version\":\"0.0.9\"," +
        $"\"advanced_networking\":{(advanced ? "true" : "false")},\"network\":{network}}}";

    public NetworkViewModelTests()
    {
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
    }

    private static FakeCli Cli(string settings) => new FakeCli()
        .Answer("settings --json", settings)
        .Fail("arm-translation status --json", "dsa: no ARM translation")
        .Fail("gpus --json", "dsa: no GPU list");

    [Fact]
    public async Task Fr28_the_page_shows_the_nat_address_dns_and_gateway_and_advanced_is_off_by_default()
    {
        var vm = new SettingsViewModel(new DsaService(Cli(Settings(false, Nat))), new KeyText());
        await vm.LoadAsync();
        Assert.False(vm.AdvancedNetworking);
        Assert.Equal(NetworkMode.Nat, vm.Network!.Mode);
        Assert.Equal("Network_ModeNat", vm.NetworkModeText);
        Assert.True(vm.HasNetworkDetails);
        Assert.Equal("172.28.16.2, fd5a:d5a:0:1::2", vm.NetworkAddresses);
        Assert.Equal("172.28.16.1", vm.NetworkDns);
        Assert.Equal("172.28.16.1", vm.NetworkGateway);
        Assert.False(vm.HasNetworkProblem);
        Assert.False(vm.NetworkRestartNeeded);
    }

    [Fact]
    public async Task Fr28_the_page_says_this_pcs_resolver_answers_androids_names_and_when_it_does_not()
    {
        var host = Nat.Replace("\"problem\":null", "\"problem\":null,\"dns_resolver\":\"host\"");
        var fallback = Nat.Replace("\"problem\":null", "\"problem\":null,\"dns_resolver\":\"network\"");
        var cli = Cli(Settings(false, host)).Answer("network --json", fallback);
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.Equal(DnsResolver.Host, vm.Network!.DnsResolver);
        Assert.Equal("Network_DnsHost", vm.NetworkDns);

        // The guest no longer reaches this PC's resolver: Android asks the network's servers.
        await vm.RefreshNetworkAsync();
        Assert.Equal(DnsResolver.Network, vm.Network!.DnsResolver);
        Assert.Equal("dns fallback: 172.28.16.1", vm.NetworkDns);
    }

    [Fact]
    public async Task Fr28_a_service_that_does_not_say_who_answers_names_shows_the_dns_servers()
    {
        var vm = new SettingsViewModel(new DsaService(Cli(Settings(false, Nat))), new KeyText());
        await vm.LoadAsync();
        Assert.Equal(DnsResolver.Unknown, vm.Network!.DnsResolver);
        Assert.Equal("172.28.16.1", vm.NetworkDns);
    }

    [Fact]
    public async Task Fr28_turning_advanced_networking_on_goes_through_the_service_and_waits_for_a_restart()
    {
        var cli = Cli(Settings(false, Nat))
            .Answer("settings set advanced-networking on --json", Settings(true, Nat));
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        await vm.SetAdvancedNetworkingAsync(true);
        Assert.Contains("settings set advanced-networking on --json", cli.Calls);
        Assert.True(vm.AdvancedNetworking);
        // Android is still behind NAT until it starts again.
        Assert.True(vm.NetworkRestartNeeded);
        // Setting what is already set sends nothing.
        await vm.SetAdvancedNetworkingAsync(true);
        Assert.Single(cli.Calls, call => call.Contains("advanced-networking"));
    }

    [Fact]
    public async Task Fr28_a_problem_with_advanced_networking_is_shown_instead_of_a_restart_notice()
    {
        var fallback = "{\"mode\":\"nat\",\"addresses\":[\"172.28.16.2\"],\"dns_servers\":[]," +
            "\"gateway\":\"172.28.16.1\",\"problem\":\"a Hyper-V virtual switch already uses the adapter\"}";
        var vm = new SettingsViewModel(new DsaService(Cli(Settings(true, fallback))), new KeyText());
        await vm.LoadAsync();
        Assert.True(vm.HasNetworkProblem);
        Assert.Contains("virtual switch", vm.NetworkProblem);
        Assert.False(vm.NetworkRestartNeeded);
        Assert.Equal("Network_None", vm.NetworkDns);
    }

    [Fact]
    public async Task Fr28_a_stopped_android_has_no_network_and_a_refresh_reads_dsa_network()
    {
        var cli = Cli(Settings(false, "{\"mode\":\"off\",\"addresses\":[],\"dns_servers\":[],\"gateway\":\"\"}"))
            .Answer("network --json", Nat);
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.Equal("Network_ModeOff", vm.NetworkModeText);
        Assert.False(vm.HasNetworkDetails);
        await vm.RefreshNetworkAsync();
        Assert.Equal("Network_ModeNat", vm.NetworkModeText);
        Assert.Equal("172.28.16.2, fd5a:d5a:0:1::2", vm.NetworkAddresses);
    }

    [Fact]
    public async Task Fr28_a_service_that_does_not_report_the_network_says_so()
    {
        var settings = "{\"render_gpu\":\"display\",\"version\":\"0.0.9\"}";
        var vm = new SettingsViewModel(new DsaService(Cli(settings)), new KeyText());
        await vm.LoadAsync();
        Assert.Null(vm.Network);
        Assert.Equal("Network_ModeUnknown", vm.NetworkModeText);
        Assert.False(vm.AdvancedNetworking);
    }

    private const string Certificates =
        "[{\"subject\":\"CN=Example Root, O=Example\",\"issuer\":\"CN=Example Root, O=Example\"," +
        "\"source\":\"machine\",\"location\":\"HKLM\\\\SOFTWARE\\\\Microsoft\\\\SystemCertificates\\\\ROOT\"," +
        "\"sha256\":\"0a1b2c\",\"not_after\":1893456000}," +
        "{\"subject\":\"CN=Proxy CA\",\"issuer\":\"CN=Corp Issuing CA\",\"source\":\"user_group_policy\"," +
        "\"location\":\"HKCU\\\\SOFTWARE\\\\Policies\\\\Microsoft\\\\SystemCertificates\\\\Root\"," +
        "\"sha256\":\"ffee\",\"not_after\":0}," +
        "{\"subject\":\"CN=Future\",\"issuer\":\"CN=Future\",\"source\":\"somewhere_new\",\"location\":\"\"," +
        "\"sha256\":\"00\",\"not_after\":0}]";

    private static string CertificateSettings(bool on, string list) =>
        Settings(false, Nat)[..^1] + $",\"host_certificates\":{(on ? "true" : "false")},\"imported_certificates\":{list}}}";

    [Fact]
    public async Task Fr28_host_certificates_switch_sends_the_setting_and_applies_at_once()
    {
        var cli = Cli(CertificateSettings(true, Certificates))
            .Answer("settings set host-certificates off --json", CertificateSettings(false, Certificates))
            .Answer("settings set host-certificates on --json", CertificateSettings(true, Certificates));
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.True(vm.HostCertificates);

        await vm.SetHostCertificatesAsync(false);
        Assert.Contains("settings set host-certificates off --json", cli.Calls);
        Assert.False(vm.HostCertificates);
        Assert.False(vm.HasError);
        // The change applies at once, so nothing asks for a restart.
        Assert.False(vm.NetworkRestartNeeded);
        // The list stays: it says what Android would trust when the switch is on again.
        Assert.Equal(3, vm.ImportedCertificates.Count);

        await vm.SetHostCertificatesAsync(true);
        Assert.Contains("settings set host-certificates on --json", cli.Calls);
        Assert.True(vm.HostCertificates);

        // Setting what is already set sends nothing.
        await vm.SetHostCertificatesAsync(true);
        Assert.Equal(2, cli.Calls.Count(call => call.Contains("host-certificates")));
    }

    [Fact]
    public async Task Fr28_imported_certificates_are_listed_with_their_subject_issuer_and_source()
    {
        var vm = new SettingsViewModel(new DsaService(Cli(CertificateSettings(true, Certificates))), new KeyText());
        await vm.LoadAsync();
        Assert.False(vm.HasNoImportedCertificates);
        var rows = vm.ImportedCertificates;
        Assert.Equal(3, rows.Count);

        Assert.Equal("CN=Example Root, O=Example", rows[0].Subject);
        Assert.Equal("CN=Example Root, O=Example", rows[0].Issuer);
        Assert.Equal("issued by CN=Example Root, O=Example", rows[0].IssuedByText);
        Assert.Equal("Certificate_SourceMachine", rows[0].SourceText);
        Assert.Equal("HKLM\\SOFTWARE\\Microsoft\\SystemCertificates\\ROOT", rows[0].Location);
        Assert.Equal("0a1b2c", rows[0].Sha256);

        Assert.Equal("CN=Proxy CA", rows[1].Subject);
        Assert.Equal("issued by CN=Corp Issuing CA", rows[1].IssuedByText);
        Assert.Equal("Certificate_SourceUserGroupPolicy", rows[1].SourceText);

        // A store a newer service names is shown as it is, and an empty location has no tooltip.
        Assert.Equal("somewhere_new", rows[2].SourceText);
        Assert.Null(rows[2].Location);
    }

    [Theory]
    [InlineData("machine", "Certificate_SourceMachine")]
    [InlineData("machine_group_policy", "Certificate_SourceMachineGroupPolicy")]
    [InlineData("machine_enterprise", "Certificate_SourceMachineEnterprise")]
    [InlineData("user", "Certificate_SourceUser")]
    [InlineData("user_group_policy", "Certificate_SourceUserGroupPolicy")]
    [InlineData("admin", "Certificate_SourceAdmin")]
    [InlineData("system", "Certificate_SourceSystem")]
    public void Fr28_every_certificate_source_the_service_names_has_a_localized_name(string source, string key)
    {
        var vm = new SettingsViewModel(new DsaService(new FakeCli()), new KeyText());
        Assert.Equal(key, vm.CertificateSourceText(source));
    }

    [Fact]
    public async Task Fr28_missing_certificate_keys_read_as_on_and_empty()
    {
        var vm = new SettingsViewModel(new DsaService(Cli(Settings(false, Nat))), new KeyText());
        Assert.True(vm.HostCertificates);
        await vm.LoadAsync();
        Assert.True(vm.HostCertificates);
        Assert.Empty(vm.ImportedCertificates);
        Assert.True(vm.HasNoImportedCertificates);
    }

    [Fact]
    public async Task Fr28_an_empty_certificate_list_says_none_were_found()
    {
        var vm = new SettingsViewModel(new DsaService(Cli(CertificateSettings(false, "[]"))), new KeyText());
        Assert.False(vm.HasNoImportedCertificates);
        await vm.LoadAsync();
        Assert.False(vm.HostCertificates);
        Assert.True(vm.HasNoImportedCertificates);
    }

    private static XDocument Strings(string file)
    {
        // The tests run from dsa/target/settings/..., outside the source tree, so each folder
        // above is tried both as the settings folder and as the repository's dsa folder.
        string[] relative = { Path.Combine("DsaSettings", file),
            Path.Combine("host", "platform", "windows", "settings", "DsaSettings", file) };
        string? found = null;
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null && found is null; dir = dir.Parent)
        {
            found = relative.Select(r => Path.Combine(dir.FullName, r)).FirstOrDefault(File.Exists);
        }
        Assert.True(found is not null, file + " was not found above " + AppContext.BaseDirectory);
        return XDocument.Load(found!);
    }

    [Theory]
    [InlineData("Strings.resx")]
    [InlineData("Strings.ko.resx")]
    public void Fr28_every_trusted_certificate_text_is_in_both_languages(string file)
    {
        var names = Strings(file).Descendants("data")
            .Select(d => (string?)d.Attribute("name"))
            .ToHashSet();
        foreach (var key in new[]
        {
            "NetworkCertificatesHeader.Text", "HostCertificatesCard.Header", "HostCertificatesCard.Description",
            "CertificatesNone.Text", "Certificate_IssuedBy", "Certificate_SourceMachine",
            "Certificate_SourceMachineGroupPolicy", "Certificate_SourceMachineEnterprise", "Certificate_SourceUser",
            "Certificate_SourceUserGroupPolicy", "Certificate_SourceAdmin", "Certificate_SourceSystem",
        })
        {
            Assert.Contains(key, names);
        }
    }
}
