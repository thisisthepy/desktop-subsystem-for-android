using System.Text.Json;
using System.Xml.Linq;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>The General page's bar says what happened in a plain sentence. What the service
/// reported, which can hold a whole HCS document, stays behind the details.</summary>
public class StopReasonTextTests
{
    private const string HcsDocument =
        "{\"Error\":-2147024809,\"ErrorMessage\":\"The parameter is incorrect.\",\"Attribution\":[{\"Memory\":{\"SizeInMB\":4096}}]}";

    private static async Task<GeneralViewModel> General(string reason)
    {
        var status = JsonSerializer.Serialize(new { state = "Stopped", last_reason = reason, update_notice = (string?)null });
        var vm = new GeneralViewModel(new DsaService(new FakeCli().Answer("status --json", status)), new KeyText());
        await vm.RefreshAsync();
        return vm;
    }

    [Theory]
    [InlineData("0x80370102", "LastReason_Virtualization")]
    [InlineData("0xc0351000", "LastReason_Virtualization")]
    [InlineData("0x80070057", "LastReason_InvalidConfiguration")]
    [InlineData("0x8037010d", "LastReason_InvalidConfiguration")]
    [InlineData("0x8007000e", "LastReason_OutOfMemory")]
    [InlineData("0x80070008", "LastReason_OutOfMemory")]
    [InlineData("0x800705aa", "LastReason_OutOfMemory")]
    [InlineData("0x800705af", "LastReason_OutOfMemory")]
    [InlineData("0x80070070", "LastReason_Disk")]
    [InlineData("0x80070027", "LastReason_Disk")]
    [InlineData("0x80070017", "LastReason_Disk")]
    [InlineData("0x8007045d", "LastReason_Disk")]
    [InlineData("0x80004005", "LastReason_General")]
    public async Task Fr17_each_mapped_code_gets_its_sentence(string code, string key)
    {
        var reason = $"the guest did not start: HcsStartComputeSystem failed (HRESULT {code}): {HcsDocument}";
        var vm = await General(reason);
        Assert.Equal(key, vm.LastReason);
        Assert.Equal(reason, vm.LastReasonDetail);
    }

    [Theory]
    [InlineData("stopped on request", "LastReason_Requested")]
    [InlineData("DSA 1.2.3 is being installed; start Android again when the service has restarted", "LastReason_Updating")]
    [InlineData("the guest did not start: the Host Compute System is not available (x); turn on the Windows feature", "LastReason_Virtualization")]
    [InlineData("the agent did not connect to the control channel within 300.0 s", "LastReason_TimedOut")]
    [InlineData("the control channel was lost: reset", "LastReason_Lost")]
    [InlineData("the guest did not start: no space left on device", "LastReason_Disk")]
    [InlineData("the Android VM exited during boot", "LastReason_General")]
    public async Task Fr17_each_known_account_gets_its_sentence(string reason, string key)
    {
        Assert.Equal(key, (await General(reason)).LastReason);
    }

    [Fact]
    public async Task Fr17_raw_json_never_reaches_the_bars_visible_text()
    {
        var vm = await General($"HcsStartComputeSystem failed, 0x80070057 {HcsDocument}");
        Assert.True(vm.HasLastReason);
        Assert.Equal("LastReason_InvalidConfiguration", vm.LastReason);
        Assert.DoesNotContain("{", vm.LastReason);
        Assert.DoesNotContain("0x", vm.LastReason);
        Assert.True(vm.HasLastReasonDetail);
        Assert.Contains(HcsDocument, vm.LastReasonDetail);
    }

    [Fact]
    public async Task Fr17_settings_in_the_hcs_document_do_not_read_as_the_failure()
    {
        // The document names Memory, which is a setting there and not what went wrong.
        var vm = await General($"the guest did not start: {HcsDocument}");
        Assert.Equal("LastReason_General", vm.LastReason);
    }

    [Fact]
    public async Task Fr17_a_hex_address_in_the_guest_console_is_not_an_error_code()
    {
        var vm = await General("the Android VM exited during boot\nlast console lines:\n  at 0x80070070");
        Assert.Equal("LastReason_General", vm.LastReason);
    }

    [Fact]
    public async Task Fr17_a_stop_on_request_has_no_details_to_show()
    {
        var vm = await General("stopped on request");
        Assert.False(vm.HasLastReasonDetail);
        Assert.Equal("", vm.LastReasonDetail);
    }

    private static Dictionary<string, string> Strings(string file)
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
        return XDocument.Load(found!).Descendants("data")
            .ToDictionary(d => (string)d.Attribute("name")!, d => d.Element("value")!.Value);
    }

    [Theory]
    [InlineData("Strings.resx")]
    [InlineData("Strings.ko.resx")]
    public void Fr17_every_sentence_exists_in_both_languages_and_fits_two_lines(string file)
    {
        var strings = Strings(file);
        foreach (var key in StopReasonText.AllKeys)
        {
            Assert.True(strings.ContainsKey(key), $"{file} has no {key}");
            Assert.InRange(strings[key].Length, 1, 120);
            Assert.DoesNotContain("0x", strings[key]);
        }
        Assert.True(strings.ContainsKey("LastReasonDetails.Header"), $"{file} has no details header");
        Assert.True(strings.ContainsKey("LastReasonCopy.ToolTip"), $"{file} has no copy label");
    }

    [Fact]
    public void Fr17_korean_sentences_end_formally()
    {
        var strings = Strings("Strings.ko.resx");
        foreach (var key in StopReasonText.AllKeys)
        {
            Assert.True(strings[key].EndsWith("니다.") || strings[key].EndsWith("십시오."), $"{key}: {strings[key]}");
        }
    }
}
