using System.Globalization;

namespace Dsa.Settings.Core;

/// <summary>The Network page: how Android reaches the network now, the advanced networking
/// switch, and the root certificates added to this computer that Android trusts.</summary>
public sealed partial class SettingsViewModel
{
    private NetworkInfo? _network;

    /// <summary>The network as last read: from <c>dsa network</c> after a refresh, else with the
    /// settings.</summary>
    public NetworkInfo? Network => _network ?? _settings?.Network;

    /// <summary>Whether Android joins this computer's network as a device of its own. Off by
    /// default.</summary>
    public bool AdvancedNetworking => _settings?.AdvancedNetworking ?? false;

    /// <summary>The mode, as the page names it.</summary>
    public string NetworkModeText => (Network?.Mode ?? NetworkMode.Unknown) switch
    {
        NetworkMode.Nat => _text.Get("Network_ModeNat"),
        NetworkMode.Advanced => _text.Get("Network_ModeAdvanced"),
        NetworkMode.Off => _text.Get("Network_ModeOff"),
        _ => _text.Get("Network_ModeUnknown"),
    };

    /// <summary>Whether Android has a network whose addresses the page can show.</summary>
    public bool HasNetworkDetails => Network is { Mode: NetworkMode.Nat or NetworkMode.Advanced };

    private string JoinOrNone(IReadOnlyList<string>? items) =>
        items is { Count: > 0 } ? string.Join(", ", items) : _text.Get("Network_None");

    public string NetworkAddresses => JoinOrNone(Network?.Addresses);

    /// <summary>Who answers Android's DNS queries: this PC's resolver, or, when that does not
    /// answer, the network's DNS servers. A service that does not say shows the servers.</summary>
    public string NetworkDns => Network?.DnsResolver switch
    {
        DnsResolver.Host => _text.Get("Network_DnsHost"),
        DnsResolver.Network => string.Format(
            CultureInfo.CurrentCulture, _text.Get("Network_DnsFallback"), JoinOrNone(Network?.DnsServers)),
        _ => JoinOrNone(Network?.DnsServers),
    };

    public string NetworkGateway => Network?.Gateway is { Length: > 0 } gateway ? gateway : _text.Get("Network_None");

    public string NetworkProblem => Network?.Problem ?? "";

    public bool HasNetworkProblem => !string.IsNullOrEmpty(Network?.Problem);

    /// <summary>Whether the switch differs from the network Android has now, which changes only
    /// when Android starts again. A problem explains a difference of its own.</summary>
    public bool NetworkRestartNeeded => Network is { Mode: NetworkMode.Nat or NetworkMode.Advanced } network
        && network.Problem is null
        && AdvancedNetworking != (network.Mode == NetworkMode.Advanced);

    /// <summary>Reads Android's network again. A service that cannot say leaves what the
    /// settings reported.</summary>
    public async Task RefreshNetworkAsync()
    {
        try
        {
            _network = await _service.NetworkAsync().ConfigureAwait(true);
        }
        catch (DsaException)
        {
            _network = null;
        }
        ChangedAll();
    }

    public async Task SetAdvancedNetworkingAsync(bool on)
    {
        if (on == AdvancedNetworking && Loaded)
        {
            return;
        }
        // The settings answer carries the network as it stands, which the page shows next.
        _network = null;
        await ChangeAsync(() => _service.SetAdvancedNetworkingAsync(on)).ConfigureAwait(true);
    }

    /// <summary>Whether Android trusts the root certificates a user or an administrator added to
    /// this computer, such as a company's or a security program's. On by default, and on for a
    /// service that does not report it.</summary>
    public bool HostCertificates => _settings?.HostCertificates ?? true;

    /// <summary>The root certificates a user or an administrator added to this computer, which
    /// Android trusts while the switch is on. Those Windows ships are not listed.</summary>
    public IReadOnlyList<CertificateRow> ImportedCertificates =>
        (_settings?.ImportedCertificates ?? Array.Empty<ImportedCertificate>())
            .Select(certificate => new CertificateRow(
                certificate.Subject,
                certificate.Issuer,
                CertificateSourceText(certificate.Source),
                string.Format(CultureInfo.CurrentCulture, _text.Get("Certificate_IssuedBy"), certificate.Issuer),
                certificate.Location is { Length: > 0 } location ? location : null,
                certificate.Sha256))
            .ToList();

    /// <summary>Whether the service found no added root certificate, so the page says so.</summary>
    public bool HasNoImportedCertificates => Loaded && ImportedCertificates.Count == 0;

    /// <summary>The store a certificate was found in, as the page names it. A name a newer
    /// service gives is shown as it is.</summary>
    public string CertificateSourceText(string source) => source switch
    {
        "machine" => _text.Get("Certificate_SourceMachine"),
        "machine_group_policy" => _text.Get("Certificate_SourceMachineGroupPolicy"),
        "machine_enterprise" => _text.Get("Certificate_SourceMachineEnterprise"),
        "user" => _text.Get("Certificate_SourceUser"),
        "user_group_policy" => _text.Get("Certificate_SourceUserGroupPolicy"),
        "admin" => _text.Get("Certificate_SourceAdmin"),
        "system" => _text.Get("Certificate_SourceSystem"),
        _ => source,
    };

    /// <summary>Lets Android trust the added root certificates, or stops it. It applies at once,
    /// so no restart is asked for.</summary>
    public Task SetHostCertificatesAsync(bool on) =>
        on == HostCertificates && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetHostCertificatesAsync(on));
}

/// <summary>One added root certificate as the Network page shows it: who it is for, who signed
/// it, the store it was found in, the line naming the issuer, the store's registry path for its
/// tooltip (null when the service gave none), and the fingerprint.</summary>
public sealed record CertificateRow(string Subject, string Issuer, string SourceText, string IssuedByText,
    string? Location, string Sha256);
