using System.Globalization;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>Answers each dsa command line with what the test set for it, printing its lines
/// one by one, and records the command lines it was given. An answer can be set again, so a
/// second run of the same line says something else.</summary>
internal sealed class FakeRunner : IDsaRunner
{
    private readonly Dictionary<string, Queue<(CliResult Result, string[] Lines)>> _answers = new();
    public List<string> Calls { get; } = new();

    public FakeRunner Answer(string line, string output, params string[] lines) =>
        Set(line, new CliResult(0, output, ""), lines);

    public FakeRunner Fail(string line, string error, params string[] lines) =>
        Set(line, new CliResult(1, "", error), lines);

    private FakeRunner Set(string line, CliResult result, string[] lines)
    {
        if (!_answers.TryGetValue(line, out var queue))
        {
            _answers[line] = queue = new Queue<(CliResult, string[])>();
        }
        queue.Enqueue((result, lines));
        return this;
    }

    public Task<CliResult> RunAsync(IReadOnlyList<string> args, Action<string>? onLine, CancellationToken cancel = default)
    {
        var line = string.Join(" ", args);
        Calls.Add(line);
        if (!_answers.TryGetValue(line, out var queue) || queue.Count == 0)
        {
            return Task.FromResult(new CliResult(2, "", $"dsa: the test has no answer for {line}"));
        }
        var (result, lines) = queue.Count > 1 ? queue.Dequeue() : queue.Peek();
        foreach (var printed in lines)
        {
            onLine?.Invoke(printed);
        }
        return Task.FromResult(result);
    }
}

/// <summary>Returns each key, with its arguments after it, so a test sees which text was
/// chosen and what was put in it.</summary>
internal sealed class ArgsText : ITextSource
{
    /// <summary>How many arguments each text takes that takes more than one.</summary>
    private static readonly Dictionary<string, int> Arity = new()
    {
        ["ApkWindow_SummaryUpdate"] = 3,
        ["ApkWindow_SummaryDowngrade"] = 3,
        ["ApkWindow_SummaryReinstall"] = 2,
        ["ApkWindow_Signer"] = 2,
        ["ApkWindow_AndroidRange"] = 2,
        ["ApkWindow_VersionText"] = 2,
        ["ApkWindow_AndroidVersion"] = 2,
        ["ApkWindow_ReasonWithWords"] = 2,
        ["ApkWindow_OpenFailed"] = 2,
        ["ApkWindow_NotApkText"] = 2,
    };

    public string Get(string key) =>
        key + " " + string.Join("|", Enumerable.Range(0, Arity.GetValueOrDefault(key, 1)).Select(i => "{" + i + "}"));
}

public class ApkInstallViewModelTests
{
    private const string Path = @"C:\Users\me\Downloads\notes.apk";

    public ApkInstallViewModelTests()
    {
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
    }

    /// <summary>What dsa apk-info prints of the notes APK, with Android in the state given.</summary>
    private static string Info(string android = "stopped", string install = "null", string installed = "null") =>
        "{\"package\":\"com.example.notes\",\"label\":\"Notes\",\"version_code\":42,\"version_name\":\"4.2\"," +
        "\"min_sdk\":26,\"min_android\":\"8.0\",\"target_sdk\":34,\"target_android\":\"14\",\"size\":3145728," +
        "\"permissions\":[{\"name\":\"android.permission.INTERNET\",\"description\":\"Have full network access\",\"sensitive\":false}," +
        "{\"name\":\"android.permission.CAMERA\",\"description\":\"Take pictures and record video\",\"sensitive\":true}]," +
        "\"icon\":{\"kind\":\"adaptive\",\"background\":{\"kind\":\"color\",\"color\":\"#FF3DDC84\"},\"foreground\":{\"kind\":\"image\",\"png\":\"iVBORw==\"}}," +
        "\"signer\":{\"scheme\":\"v2\",\"sha256\":\"AB:CD\"},\"abis\":[\"x86_64\"]," +
        $"\"android\":\"{android}\",\"installed\":{installed},\"install\":{install}}}";

    private const string Installed41 = "{\"version_code\":41,\"version_name\":\"4.1\",\"signers\":[\"AB:CD\"]}";

    private static async Task<ApkInstallViewModel> Loaded(FakeRunner dsa)
    {
        var vm = new ApkInstallViewModel(dsa, new ArgsText(), Path);
        await vm.LoadAsync();
        return vm;
    }

    [Fact]
    public async Task Fr11_the_window_shows_the_apks_name_icon_version_and_permissions_before_android_starts()
    {
        var dsa = new FakeRunner().Answer($"apk-info {Path} --json", Info());
        var vm = await Loaded(dsa);
        Assert.Equal(ApkInstallPage.Details, vm.Page);
        Assert.Equal("Notes", vm.Title);
        Assert.Equal("com.example.notes", vm.Subtitle);
        Assert.StartsWith("ApkWindow_VersionChip 4.2", vm.VersionChip);
        Assert.Equal("3 MB", vm.SizeChip);
        Assert.StartsWith("ApkWindow_AndroidChip 8.0", vm.AndroidChip);
        Assert.True(vm.Apk!.Icon!.IsAdaptive);
        Assert.Equal(0xFF3DDC84u, vm.Apk.Icon.Background!.Color);
        // Permissions Android asks the user about come first.
        Assert.Equal("Take pictures and record video", vm.Permissions[0].Description);
        Assert.True(vm.Permissions[0].Sensitive);
        Assert.Contains(vm.Facts, f => f.Value.Contains("AB:CD"));
        Assert.Equal(ApkInstallKind.Unknown, vm.Kind);
        Assert.StartsWith("ApkWindow_SummaryUnknown", vm.Summary);
        Assert.True(vm.CanInstall);
        Assert.StartsWith("ApkWindow_Install ", vm.InstallButton);
        // Reading the file never starts Android.
        Assert.Equal(new[] { $"apk-info {Path} --json" }, dsa.Calls);
    }

    [Fact]
    public async Task Fr11_installing_starts_android_shows_progress_and_ends_with_open()
    {
        var dsa = new FakeRunner()
            .Answer($"apk-info {Path} --json", Info())
            .Answer("start --wait --json", "{\"state\":\"Active\"}")
            .Answer($"apk-info {Path} --json", Info("running", "\"new\""))
            .Answer($"install {Path} --progress --json", "{\"installed\":true}",
                "stage sending", "progress 50", "progress 100", "stage installing")
            .Answer("launch com.example.notes", "Launching com.example.notes");
        var vm = await Loaded(dsa);
        var pages = new List<(ApkInstallPage, int?)>();
        vm.PropertyChanged += (_, _) => pages.Add((vm.Page, vm.ProgressPercent));
        await vm.InstallAsync();
        Assert.Contains((ApkInstallPage.Progress, (int?)null), pages);
        Assert.Contains((ApkInstallPage.Progress, (int?)50), pages);
        Assert.Equal(ApkInstallPage.Done, vm.Page);
        Assert.StartsWith("ApkWindow_DoneTitle Notes", vm.ResultTitle);
        Assert.True(vm.CanOpen);
        Assert.Null(await vm.OpenAsync());
        Assert.Equal(new[]
        {
            $"apk-info {Path} --json",
            "start --wait --json",
            $"apk-info {Path} --json",
            $"install {Path} --progress --json",
            "launch com.example.notes",
        }, dsa.Calls);
    }

    [Fact]
    public async Task Fr11_an_update_found_once_android_runs_shows_both_versions_and_waits_for_the_user()
    {
        var dsa = new FakeRunner()
            .Answer($"apk-info {Path} --json", Info())
            .Answer("start --wait --json", "{\"state\":\"Active\"}")
            .Answer($"apk-info {Path} --json", Info("running", "\"update\"", Installed41))
            .Answer($"install {Path} --progress --json", "{\"installed\":true}");
        var vm = await Loaded(dsa);
        await vm.InstallAsync();
        Assert.Equal(ApkInstallPage.Details, vm.Page);
        Assert.Equal(ApkInstallKind.Update, vm.Kind);
        Assert.StartsWith("ApkWindow_SummaryUpdate Notes|ApkWindow_VersionText 4.1|41", vm.Summary.Replace("|ApkWindow_VersionText 4.2|42", ""));
        Assert.Contains("4.2", vm.Summary);
        Assert.StartsWith("ApkWindow_Update ", vm.InstallButton);
        Assert.Contains(vm.Facts, f => f.Name.StartsWith("ApkWindow_FactInstalled") && f.Value.Contains("4.1"));
        Assert.DoesNotContain($"install {Path} --progress --json", dsa.Calls);

        await vm.InstallAsync();
        Assert.Equal(ApkInstallPage.Done, vm.Page);
        Assert.StartsWith("ApkWindow_UpdatedTitle", vm.ResultTitle);
    }

    [Theory]
    [InlineData("\"downgrade\"", "ApkWindow_SummaryDowngrade")]
    [InlineData("\"signature-mismatch\"", "ApkWindow_SummarySignature")]
    public async Task Fr11_a_downgrade_or_another_signer_is_refused_with_its_reason(string kind, string summary)
    {
        var dsa = new FakeRunner().Answer($"apk-info {Path} --json", Info("running", kind, Installed41));
        var vm = await Loaded(dsa);
        Assert.True(vm.IsBlocked);
        Assert.False(vm.CanInstall);
        Assert.StartsWith(summary, vm.Summary);
        await vm.InstallAsync();
        Assert.DoesNotContain(dsa.Calls, c => c.StartsWith("install"));
    }

    [Fact]
    public async Task Fr11_a_failure_shows_its_reason_in_plain_words()
    {
        var dsa = new FakeRunner()
            .Answer($"apk-info {Path} --json", Info("running", "\"new\""))
            .Fail($"install {Path} --progress --json",
                "dsa: not installed (incompatible): notes.apk: The app does not run on this Android (com.example.notes). Android said: INSTALL_FAILED_NO_MATCHING_ABIS",
                "stage sending", "progress 100", "stage installing");
        var vm = await Loaded(dsa);
        await vm.InstallAsync();
        Assert.Equal(ApkInstallPage.Failed, vm.Page);
        Assert.StartsWith("ApkWindow_FailedTitle", vm.ResultTitle);
        Assert.StartsWith("ApkWindow_ReasonWithWords ApkWindow_Reason_incompatible", vm.ResultText);
        Assert.Contains("INSTALL_FAILED_NO_MATCHING_ABIS", vm.ResultText);
    }

    [Fact]
    public async Task Fr11_a_file_that_is_not_an_apk_says_why()
    {
        var dsa = new FakeRunner().Fail($"apk-info {Path} --json", "dsa: the file is not a ZIP archive, so it is not an APK");
        var vm = await Loaded(dsa);
        Assert.Equal(ApkInstallPage.Failed, vm.Page);
        Assert.StartsWith("ApkWindow_NotApkTitle", vm.ResultTitle);
        Assert.Contains("not a ZIP archive", vm.ResultText);
        Assert.False(vm.CanInstall);
    }

    [Fact]
    public async Task Fr11_android_that_does_not_start_is_a_failure_with_its_reason()
    {
        var dsa = new FakeRunner()
            .Answer($"apk-info {Path} --json", Info())
            .Fail("start --wait --json", "dsa: Android did not start: the image is missing");
        var vm = await Loaded(dsa);
        await vm.InstallAsync();
        Assert.Equal(ApkInstallPage.Failed, vm.Page);
        Assert.StartsWith("ApkWindow_StartFailed Android did not start: the image is missing", vm.ResultText);
    }

    [Fact]
    public void Fr11_sizes_read_in_the_users_units()
    {
        Assert.Equal("512 KB", ApkInstallViewModel.Size(512 * 1024));
        Assert.Equal("1.5 MB", ApkInstallViewModel.Size(3 * 512 * 1024));
        Assert.Equal("2 GB", ApkInstallViewModel.Size(2L * 1024 * 1024 * 1024));
    }
}
