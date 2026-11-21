using System.Globalization;
using System.Xml.Linq;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>The Display page's Vulkan driver: auto, native or Dozen, what the renderer runs on
/// now, and the Compatibility Pack offered where Android has nothing to draw on.</summary>
public class VulkanDriverTests
{
    private const string Auto =
        "{\"render_gpu\":\"display\",\"freeze\":false,\"program_updates\":true,\"image_updates\":true," +
        "\"program_updates_available\":true,\"image_updates_available\":false,\"version\":\"0.0.9\"," +
        "\"vulkan_driver\":\"auto\",\"graphics_state\":\"native\",\"graphics_detail\":\"rendering on Intel(R) Arc(TM) Graphics\"}";

    public VulkanDriverTests()
    {
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
    }

    private static FakeCli Cli(string settings) => new FakeCli()
        .Answer("settings --json", settings)
        .Fail("gpus --json", "dsa: no GPU list");

    [Fact]
    public async Task Pr3_the_vulkan_driver_is_auto_by_default_and_the_list_changes_it()
    {
        var cli = Cli(Auto)
            .Answer("settings set vulkan-driver dozen --json", Auto.Replace("\"vulkan_driver\":\"auto\"", "\"vulkan_driver\":\"dozen\""))
            .Answer("settings set vulkan-driver native --json", Auto.Replace("\"vulkan_driver\":\"auto\"", "\"vulkan_driver\":\"native\""));
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.Equal("auto", vm.VulkanDriver);
        Assert.Equal(0, vm.VulkanDriverIndex);
        Assert.Equal(3, vm.VulkanDriverChoices.Count);
        Assert.Equal(GraphicsState.Native, vm.Graphics);
        Assert.Equal("Graphics_Native (rendering on Intel(R) Arc(TM) Graphics)", vm.GraphicsStatusText);
        Assert.False(vm.OffersCompatibilityPack);

        await vm.SetVulkanDriverAsync(2);
        Assert.Equal("dozen", vm.VulkanDriver);
        Assert.True(vm.OffersCompatibilityPack, "Dozen asked for: the pack is offered");
        await vm.SetVulkanDriverAsync(1);
        Assert.Equal("native", vm.VulkanDriver);
        Assert.Contains("settings set vulkan-driver dozen --json", cli.Calls);
        Assert.Contains("settings set vulkan-driver native --json", cli.Calls);

        // The same value, or one out of the list, sends nothing.
        var calls = cli.Calls.Count;
        await vm.SetVulkanDriverAsync(1);
        await vm.SetVulkanDriverAsync(7);
        Assert.Equal(calls, cli.Calls.Count);
    }

    [Fact]
    public async Task Pr3_nothing_to_draw_on_offers_the_pack_and_the_cpu_warns()
    {
        var none = Auto.Replace("\"graphics_state\":\"native\"", "\"graphics_state\":\"unavailable\"");
        var vm = new SettingsViewModel(new DsaService(Cli(none)), new KeyText());
        await vm.LoadAsync();
        Assert.Equal(GraphicsState.Unavailable, vm.Graphics);
        Assert.True(vm.OffersCompatibilityPack);
        Assert.False(vm.DrawsOnCpu);
        Assert.Equal("ms-windows-store://pdp/?ProductId=9NQPSL29BFFF", SettingsViewModel.CompatibilityPackStoreUri);

        var cpu = Auto.Replace("\"graphics_state\":\"native\"", "\"graphics_state\":\"dozen-cpu\"");
        var onCpu = new SettingsViewModel(new DsaService(Cli(cpu)), new KeyText());
        await onCpu.LoadAsync();
        Assert.True(onCpu.DrawsOnCpu);
        Assert.False(onCpu.OffersCompatibilityPack);
    }

    [Fact]
    public async Task Pr3_a_service_that_predates_the_setting_reads_as_auto_and_unknown()
    {
        var old = "{\"render_gpu\":\"display\",\"version\":\"0.0.9\"}";
        var vm = new SettingsViewModel(new DsaService(Cli(old)), new KeyText());
        await vm.LoadAsync();
        Assert.Equal("auto", vm.VulkanDriver);
        Assert.Equal(GraphicsState.Unknown, vm.Graphics);
        Assert.Equal("Graphics_Unknown", vm.GraphicsStatusText);
    }

    [Theory]
    [InlineData("native", GraphicsState.Native)]
    [InlineData("dozen", GraphicsState.Dozen)]
    [InlineData("dozen-cpu", GraphicsState.DozenCpu)]
    [InlineData("unavailable", GraphicsState.Unavailable)]
    [InlineData("something-new", GraphicsState.Unknown)]
    [InlineData(null, GraphicsState.Unknown)]
    public void Pr3_each_graphics_state_name_is_read(string? name, GraphicsState state) =>
        Assert.Equal(state, DsaService.ReadGraphicsState(name));

    private static XDocument Strings(string file)
    {
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
    [MemberData(nameof(StringTables.Files), MemberType = typeof(StringTables))]
    public void Pr3_every_vulkan_driver_text_is_in_every_language(string file)
    {
        var values = Strings(file).Descendants("data")
            .ToDictionary(d => (string?)d.Attribute("name") ?? "", d => d.Element("value")?.Value ?? "");
        foreach (var name in new[] { "VulkanDriverCard.Header", "VulkanDriverCard.Description",
            "Vulkan_Auto", "Vulkan_Native", "Vulkan_Dozen", "Graphics_Unknown", "Graphics_Native",
            "Graphics_Dozen", "Graphics_DozenCpu", "Graphics_Unavailable", "Graphics_WithDetail",
            "CompatibilityPackLink.Content", "CpuRenderingBar.Title", "CpuRenderingBar.Message" })
        {
            Assert.True(values.ContainsKey(name), $"{file} has no {name}");
        }
        Assert.Contains("Microsoft.OpenCLGLVulkanCompatibilityPack", values["Graphics_Unavailable"]);
    }
}
