using System.Globalization;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>The controls the settings app offers for what was before only a dsa command: the
/// memory limit, the trace switches and freezing now.</summary>
public class CliParityTests
{
    private const string Settings =
        "{\"render_gpu\":\"display\",\"freeze\":false,\"program_updates\":true,\"image_updates\":true," +
        "\"program_updates_available\":true,\"image_updates_available\":false,\"version\":\"0.0.9\"," +
        "\"input_trace\":false,\"resize_trace\":false,\"memory_limit_mb\":0," +
        "\"developer_mode\":false,\"adb_port\":58526}";

    public CliParityTests()
    {
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
    }

    private static FakeCli Cli() => new FakeCli()
        .Answer("settings --json", Settings)
        .Fail("gpus --json", "dsa: no GPU list");

    [Fact]
    public async Task Fr17_memory_limit_is_automatic_by_default_and_set_through_the_service()
    {
        var cli = Cli()
            .Answer("settings set memory-limit 8192 --json", Settings.Replace("\"memory_limit_mb\":0", "\"memory_limit_mb\":8192"))
            .Answer("settings set memory-limit auto --json", Settings);
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.Equal(0, vm.MemoryLimitMb);
        Assert.Equal("MemoryLimit_Auto", vm.MemoryLimitText);
        Assert.Equal(0, vm.SelectedMemoryLimitIndex);

        await vm.SetMemoryLimitAsync(8192);
        Assert.Equal(8192, vm.MemoryLimitMb);
        Assert.Equal("8 GB", vm.MemoryLimitText);
        Assert.Equal(8192, vm.MemoryLimitChoices[vm.SelectedMemoryLimitIndex].Mb);

        await vm.SetMemoryLimitAsync(0);
        Assert.Equal(0, vm.MemoryLimitMb);
        Assert.Contains("settings set memory-limit auto --json", cli.Calls);
    }

    [Fact]
    public async Task Fr17_a_memory_limit_too_small_to_start_android_is_not_sent()
    {
        var cli = Cli();
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        await vm.SetMemoryLimitAsync(1024);
        Assert.DoesNotContain(cli.Calls, call => call.Contains("memory-limit"));
        Assert.Equal(0, vm.MemoryLimitMb);
    }

    [Fact]
    public async Task Fr17_a_memory_limit_set_from_the_command_line_shows_as_chosen()
    {
        var cli = new FakeCli()
            .Answer("settings --json", Settings.Replace("\"memory_limit_mb\":0", "\"memory_limit_mb\":5000"))
            .Fail("gpus --json", "dsa: no GPU list");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.Equal("5000 MB", vm.MemoryLimitText);
        var chosen = vm.MemoryLimitChoices[vm.SelectedMemoryLimitIndex];
        Assert.Equal(5000, chosen.Mb);
        Assert.Equal(0, vm.MemoryLimitChoices[0].Mb);
        Assert.True(vm.MemoryLimitChoices.Skip(1).Select(c => c.Mb).SequenceEqual(
            vm.MemoryLimitChoices.Skip(1).Select(c => c.Mb).OrderBy(mb => mb)));
    }

    [Fact]
    public async Task Fr19_trace_switches_are_off_by_default_and_need_an_administrator()
    {
        var cli = Cli()
            .Answer("settings set input-trace on --json", Settings.Replace("\"input_trace\":false", "\"input_trace\":true"))
            .Answer("settings set resize-trace on --json", Settings.Replace("\"resize_trace\":false", "\"resize_trace\":true"));
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.False(vm.InputTrace);
        Assert.False(vm.ResizeTrace);
        Assert.False(vm.CanChangeTraces);
        Assert.True(vm.TracesNeedAdministrator);

        vm.IsAdministrator = true;
        Assert.True(vm.CanChangeTraces);
        Assert.False(vm.TracesNeedAdministrator);
        await vm.SetInputTraceAsync(true);
        Assert.True(vm.InputTrace);
        await vm.SetResizeTraceAsync(true);
        Assert.True(vm.ResizeTrace);
        Assert.Contains("settings set input-trace on --json", cli.Calls);
        Assert.Contains("settings set resize-trace on --json", cli.Calls);
    }

    [Fact]
    public async Task Fr20_the_adb_address_follows_the_port_while_developer_mode_is_off()
    {
        var cli = new FakeCli()
            .Answer("settings --json", Settings.Replace("58526", "5555"))
            .Fail("gpus --json", "dsa: no GPU list");
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.False(vm.DeveloperMode);
        Assert.Equal(5555, vm.AdbPort);
        Assert.Equal("adb connect 127.0.0.1:5555", vm.AdbConnect);
    }

    [Fact]
    public async Task Fr17_memory_page_shows_the_vm_size_guest_use_and_kernel_breakdown()
    {
        var cli = new FakeCli().Answer("memory --json",
            "{\"host\":[{\"role\":\"vm\",\"pid\":10,\"working_set_kb\":1048576,\"private_kb\":1000}]," +
            "\"host_total_kb\":1048576,\"vm_beyond_guest_kb\":262144," +
            "\"guest\":{\"mem_total_kb\":2097152,\"mem_available_kb\":1310720,\"mem_free_kb\":1000000," +
            "\"age_seconds\":3,\"top_processes\":[]," +
            "\"kernel\":{\"slab_unreclaimable_kb\":51200,\"kernel_stack_kb\":10240,\"page_tables_kb\":20480," +
            "\"vmalloc_used_kb\":30720,\"percpu_kb\":1024,\"cma_total_kb\":0,\"mem_online_kb\":2228224," +
            "\"unaccounted_kb\":40960,\"reserved_at_boot_kb\":131072}}}");
        var vm = new MemoryViewModel(new DsaService(cli), new KeyText());
        await vm.RefreshAsync();
        Assert.Equal("1.00 GB", vm.VmSize);
        Assert.Equal("768.0 MB", vm.GuestUsed);
        Assert.Equal("256.0 MB", vm.VmBeyondGuest);
        Assert.True(vm.KernelReported);
        Assert.Equal("50.0 MB", vm.KernelRows.Single(r => r.Label == "Memory_KernelSlab").Size);
        Assert.Equal("40.0 MB", vm.KernelRows.Single(r => r.Label == "Memory_KernelUnaccounted").Size);
        Assert.Equal("128.0 MB", vm.KernelRows.Single(r => r.Label == "Memory_KernelReserved").Size);
    }

    [Fact]
    public async Task Nfr1_memory_page_leaves_out_the_disk_pages_the_vm_maps()
    {
        // 1336 MiB resident, 478 MiB of them pages of the directly mapped disks.
        var cli = new FakeCli().Answer("memory --json",
            "{\"host\":[{\"role\":\"vm\",\"pid\":10,\"working_set_kb\":1368064,\"private_kb\":1000000," +
            "\"mapped_kb\":489472},{\"role\":\"window manager\",\"pid\":20,\"working_set_kb\":40960," +
            "\"private_kb\":30000}],\"host_total_kb\":1409024,\"host_mapped_kb\":489472," +
            "\"guest\":{\"mem_total_kb\":2097152,\"top_processes\":[]}}");
        var vm = new MemoryViewModel(new DsaService(cli), new KeyText());
        await vm.RefreshAsync();
        Assert.Equal("858.0 MB", vm.VmSize);
        Assert.Equal("898.0 MB", vm.HostTotal);
        Assert.Equal("858.0 MB", vm.HostRows[0].Size);
        Assert.Equal("40.0 MB", vm.HostRows[1].Size);
    }

    [Fact]
    public async Task Fr17_memory_page_without_a_kernel_report_shows_no_kernel_rows()
    {
        var cli = new FakeCli().Answer("memory --json",
            "{\"host\":[],\"host_total_kb\":0,\"guest\":{\"mem_total_kb\":1024,\"top_processes\":[]}}");
        var vm = new MemoryViewModel(new DsaService(cli), new KeyText());
        await vm.RefreshAsync();
        Assert.False(vm.KernelReported);
        Assert.Empty(vm.KernelRows);
        Assert.False(vm.HasVmSize);
        Assert.False(vm.HasVmBeyondGuest);
    }

    [Fact]
    public async Task Fr17_general_page_freezes_a_running_android_now()
    {
        var cli = new FakeCli()
            .Answer("status --json", "{\"state\":\"Active\"}")
            .Answer("freeze --json", "{\"state\":\"Frozen\"}");
        var vm = new GeneralViewModel(new DsaService(cli), new KeyText());
        await vm.RefreshAsync();
        Assert.True(vm.CanFreeze);
        await vm.FreezeAsync();
        Assert.Equal(SessionState.Frozen, vm.State);
        Assert.False(vm.CanFreeze);
        Assert.Contains("freeze --json", cli.Calls);
    }
    [Fact]
    public async Task Fr15_the_release_channel_is_chosen_through_the_service_by_an_administrator()
    {
        var channels = Settings.Replace("\"program_updates_available\":true",
            "\"program_updates_available\":true,\"update_channel_available\":true,\"prerelease_updates\":true");
        var cli = new FakeCli()
            .Answer("settings --json", channels)
            .Fail("gpus --json", "dsa: no GPU list")
            .Answer("settings set prerelease-updates off --json",
                channels.Replace("\"prerelease_updates\":true", "\"prerelease_updates\":false"));
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        Assert.True(vm.PrereleaseUpdates, "a pre-release installation starts on the pre-release channel");
        Assert.True(vm.UpdateChannelAvailable);
        Assert.False(vm.CanChangePrereleaseUpdates);
        Assert.True(vm.PrereleaseUpdatesNeedAdministrator);
        vm.IsAdministrator = true;
        Assert.True(vm.CanChangePrereleaseUpdates);
        await vm.SetPrereleaseUpdatesAsync(false);
        Assert.False(vm.PrereleaseUpdates);
        Assert.Contains("settings set prerelease-updates off --json", cli.Calls);

        // A service from before the channels says nothing: no channel to choose.
        var older = new SettingsViewModel(new DsaService(Cli()), new KeyText()) { IsAdministrator = true };
        await older.LoadAsync();
        Assert.False(older.UpdateChannelAvailable);
        Assert.False(older.CanChangePrereleaseUpdates);
    }
}
