using Xunit;

namespace Dsa.Settings.Core.Tests;

public class DeviceIdentityViewModelTests
{
    private const string Effective =
        "\"manufacturer\":\"thisisthepy\",\"brand\":\"DSA\",\"model\":\"Desktop Subsystem for Android\"," +
        "\"device\":\"dsa_x86_64\",\"name\":\"dsa_x86_64\",\"characteristics\":\"tablet\"," +
        "\"fingerprint\":\"DSA/dsa_x86_64/dsa_x86_64:16/BP2A/1.dsaab:user/release-keys\"," +
        "\"fingerprint_release\":\"16\",\"fingerprint_id\":\"BP2A\",\"fingerprint_incremental\":\"1.dsaab\"," +
        "\"fingerprint_type\":\"user\",\"fingerprint_tags\":\"release-keys\"";

    private static string Report(string chosen, string effective) =>
        $"{{\"chosen\":{{{chosen}}},\"effective\":{{{effective}}}}}";

    private static readonly string Defaults = Report("\"model\":\"\"", Effective);

    private static FakeCli Cli(string state = "Stopped") => new FakeCli()
        .Answer("settings device --json", Defaults)
        .Answer("status --json", $"{{\"state\":\"{state}\",\"last_reason\":null,\"update_notice\":null,\"problem\":null}}");

    private static DeviceIdentityViewModel Model(FakeCli cli) => new(cli, new DsaService(cli), new KeyText());

    [Fact]
    public async Task Fr17_the_page_shows_each_value_android_reports_and_the_fingerprint()
    {
        var vm = Model(Cli());
        await vm.LoadAsync();
        Assert.False(vm.HasError);
        Assert.Equal("Desktop Subsystem for Android", vm.Field("model").Placeholder);
        Assert.Equal("", vm.Field("model").Value);
        Assert.Equal("tablet", vm.Field("characteristics").Placeholder);
        Assert.Equal("DSA/dsa_x86_64/dsa_x86_64:16/BP2A/1.dsaab:user/release-keys", vm.FingerprintPreview);
        Assert.Equal(6, vm.Names.Count);
        Assert.Equal(5, vm.FingerprintParts.Count);
        Assert.False(vm.CanSave, "nothing changed");

        // The preview follows what is typed.
        vm.Field("brand").Value = "mine";
        vm.Field("fingerprint.type").Value = "userdebug";
        Assert.Equal("mine/dsa_x86_64/dsa_x86_64:16/BP2A/1.dsaab:userdebug/release-keys", vm.FingerprintPreview);
        vm.Field("fingerprint").Value = "a/b/c:1/2/3:user/release-keys";
        Assert.Equal("a/b/c:1/2/3:user/release-keys", vm.FingerprintPreview);
    }

    [Fact]
    public async Task Fr17_saving_sends_each_changed_value_and_offers_a_restart_while_android_runs()
    {
        var cli = Cli("Active")
            .Answer("settings set device.model My tablet --json", Report("\"model\":\"My tablet\"", Effective))
            .Answer("settings reset device.brand --json", Report("\"model\":\"My tablet\"", Effective));
        var vm = Model(cli);
        await vm.LoadAsync();
        vm.Field("model").Value = "My tablet";
        Assert.True(vm.CanSave);
        Assert.False(vm.NeedsRestart);
        await vm.SaveAsync();
        Assert.False(vm.HasError, vm.Error);
        Assert.Contains("settings set device.model My tablet --json", cli.Calls);
        Assert.Equal("My tablet", vm.Field("model").Value);
        Assert.False(vm.Field("model").IsEdited);
        Assert.True(vm.NeedsRestart);
        Assert.Equal("Identity_SavedRestart", vm.Message);

        cli.Answer("stop --wait --timeout 120 --json", "{\"state\":\"Stopped\"}")
            .Answer("start --wait --timeout 360 --json", "{\"state\":\"Active\"}");
        await vm.RestartAsync();
        Assert.False(vm.HasError, vm.Error);
        Assert.False(vm.NeedsRestart);
        Assert.Equal("Identity_Restarted", vm.Message);
    }

    [Fact]
    public async Task Fr17_a_value_that_is_not_allowed_is_shown_and_never_sent()
    {
        var cli = Cli();
        var vm = Model(cli);
        await vm.LoadAsync();
        vm.Field("brand").Value = "two words";
        Assert.True(vm.Field("brand").HasProblem);
        Assert.Equal("Identity_brand_Invalid", vm.Field("brand").Problem);
        Assert.False(vm.CanSave);
        await vm.SaveAsync();
        Assert.DoesNotContain(cli.Calls, c => c.StartsWith("settings set", StringComparison.Ordinal));

        Assert.Null(DeviceIdentityRules.Check("model", "Desktop Subsystem for Android"));
        Assert.NotNull(DeviceIdentityRules.Check("model", " padded "));
        Assert.NotNull(DeviceIdentityRules.Check("characteristics", "Tablet"));
        Assert.Null(DeviceIdentityRules.Check("characteristics", "tablet,nosdcard"));
        Assert.NotNull(DeviceIdentityRules.Check("fingerprint.type", "root"));
        Assert.True(DeviceIdentityRules.IsFingerprint("google/oriole/oriole:14/UQ1A.240105.004/11206848:user/release-keys"));
        Assert.False(DeviceIdentityRules.IsFingerprint("DSA/dsa:16/ID/1:user/release-keys"));
        Assert.Null(DeviceIdentityRules.Check("brand", ""));
    }

    [Fact]
    public async Task Fr17_reset_restores_the_defaults()
    {
        var cli = Cli().Answer("settings reset device --json", Defaults);
        var vm = Model(cli);
        await vm.LoadAsync();
        await vm.ResetAsync();
        Assert.False(vm.HasError, vm.Error);
        Assert.Contains("settings reset device --json", cli.Calls);
        Assert.Equal("", vm.Field("model").Value);
        Assert.Equal("Identity_Reset", vm.Message);
    }

    private const string ComputerReport =
        "{\"chosen\":{\"model\":\"\"}," +
        "\"effective\":{\"manufacturer\":\"Samsung\",\"brand\":\"Samsung\",\"model\":\"NT960XGP-G72AG\"," +
        "\"device\":\"dsa_x86_64\",\"name\":\"dsa_x86_64\",\"characteristics\":\"tablet\"," +
        "\"model_name\":\"Galaxy Book4 Ultra\",\"device_name\":\"BREW-BOOK\",\"soc_manufacturer\":\"Intel\",\"soc_model\":\"\"}," +
        "\"sources\":{\"manufacturer\":\"computer\",\"brand\":\"computer\",\"model\":\"computer\",\"device\":\"dsa\"," +
        "\"model_name\":\"computer\",\"device_name\":\"computer\",\"soc_manufacturer\":\"computer\",\"soc_model\":\"dsa\"}}";

    [Fact]
    public async Task Fr17_the_page_shows_the_computers_values_and_where_each_default_came_from()
    {
        var cli = new FakeCli()
            .Answer("settings device --json", ComputerReport)
            .Answer("status --json", "{\"state\":\"Stopped\",\"last_reason\":null,\"update_notice\":null,\"problem\":null}");
        var vm = Model(cli);
        await vm.LoadAsync();
        Assert.False(vm.HasError, vm.Error);
        Assert.Equal("NT960XGP-G72AG", vm.Field("model").Placeholder);
        Assert.Equal("computer", vm.Field("model").Origin);
        Assert.Equal("Identity_FromComputer", vm.Field("model").OriginText);
        Assert.Equal("dsa", vm.Field("device").Origin);
        Assert.Equal("Identity_FromDsa", vm.Field("device").OriginText);
        // A source the report does not name is DSA's.
        Assert.Equal("dsa", vm.Field("fingerprint.type").Origin);

        Assert.Equal(new[] { "model_name", "device_name", "soc_manufacturer", "soc_model" },
            vm.ComputerValues.Select(v => v.Key));
        Assert.Equal("Galaxy Book4 Ultra", vm.Computer("model_name").Value);
        Assert.Equal("BREW-BOOK", vm.Computer("device_name").Value);
        Assert.Equal("Identity_FromComputer", vm.Computer("device_name").OriginText);
        Assert.Equal("Intel", vm.Computer("soc_manufacturer").Value);
        // A value the computer does not say says so.
        Assert.Equal("Identity_NotSaid", vm.Computer("soc_model").Value);
        Assert.Equal("Identity_FromDsa", vm.Computer("soc_model").OriginText);
    }

    [Fact]
    public async Task Fr17_reset_returns_a_field_to_the_computers_value()
    {
        var cli = new FakeCli()
            .Answer("settings device --json", ComputerReport.Replace("{\"model\":\"\"}", "{\"model\":\"My tablet\"}"))
            .Answer("status --json", "{\"state\":\"Stopped\",\"last_reason\":null,\"update_notice\":null,\"problem\":null}")
            .Answer("settings reset device --json", ComputerReport);
        var vm = Model(cli);
        await vm.LoadAsync();
        Assert.Equal("My tablet", vm.Field("model").Value);
        await vm.ResetAsync();
        Assert.Equal("", vm.Field("model").Value);
        Assert.Equal("NT960XGP-G72AG", vm.Field("model").Placeholder);
    }

    [Fact]
    public async Task Fr17_a_refusal_from_the_service_is_shown()
    {
        var cli = Cli().Fail("settings set device.model X --json", "dsa: the model is 1 to 64 characters long");
        var vm = Model(cli);
        await vm.LoadAsync();
        vm.Field("model").Value = "X";
        await vm.SaveAsync();
        Assert.True(vm.HasError);
        Assert.Contains("the model is 1 to 64 characters long", vm.Error);
    }
}
