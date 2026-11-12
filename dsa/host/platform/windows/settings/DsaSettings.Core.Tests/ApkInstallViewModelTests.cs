using System.Globalization;
using System.Text.RegularExpressions;
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
        ["ApkWindow_BundleSummary"] = 3,
        ["ApkWindow_BundleSummaryObb"] = 3,
        ["ApkWindow_ApksValue"] = 2,
        ["ApkWindow_Signer"] = 2,
        ["ApkWindow_AndroidRange"] = 2,
        ["ApkWindow_VersionText"] = 2,
        ["ApkWindow_AndroidVersion"] = 2,
        ["ApkWindow_ReasonWithWords"] = 2,
        ["ApkWindow_OpenFailed"] = 2,
        ["ApkWindow_LaunchFailedAfterInstall"] = 2,
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

    /// <summary>The processor types of the PC the tests stand for: an x64 PC.</summary>
    private static readonly string[] X64 = { "x86_64", "x86" };

    private static async Task<ApkInstallViewModel> Loaded(FakeRunner dsa, bool launchWhenReady = false)
    {
        var vm = new ApkInstallViewModel(dsa, new ArgsText(), Path, X64) { LaunchWhenReady = launchWhenReady };
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
        Assert.Equal("Version: 4.2", vm.VersionLine);
        Assert.Equal("3 MB", vm.SizeChip);
        Assert.StartsWith("ApkWindow_AndroidChip 8.0", vm.AndroidChip);
        Assert.True(vm.Apk!.Icon!.IsAdaptive);
        Assert.Equal(0xFF3DDC84u, vm.Apk.Icon.Background!.Color);
        // Permissions Android asks the user about come first.
        Assert.Equal("Take pictures and record video", vm.Permissions[0].Description);
        Assert.True(vm.Permissions[0].Sensitive);
        Assert.Contains(vm.Facts, f => f.Value.Contains("AB:CD"));
        Assert.Equal(ApkInstallKind.Unknown, vm.Kind);
        // Android starting is shown when it happens; nothing needs saying before that.
        Assert.Equal("", vm.Summary);
        Assert.True(vm.CanInstall);
        Assert.StartsWith("ApkWindow_Install ", vm.InstallButton);
        // Reading the file never starts Android.
        Assert.Equal(new[] { $"apk-info {Path} --json" }, dsa.Calls);
    }

    /// <summary>What dsa apk-info prints of an APKPure bundle of the notes app: its base
    /// APK's information, and the three of its APKs and the expansion file installing it sends.</summary>
    private static string BundleInfo(string android = "stopped", string install = "null") =>
        Info(android, install).TrimEnd('}') + ",\"format\":\"xapk\"," +
        "\"apks\":[\"base.apk\",\"split_config.x86_64.apk\",\"split_config.en.apk\"],\"split_count\":2," +
        "\"obbs\":[\"main.42.com.example.notes.obb\"],\"total_size\":10485760,\"install_abi\":\"x86_64\"}";

    [Fact]
    public async Task Fr11_a_bundle_shows_its_base_app_with_how_many_files_it_installs_and_their_size()
    {
        const string bundle = @"C:\Users\me\Downloads\notes.xapk";
        var dsa = new FakeRunner().Answer($"apk-info {bundle} --json", BundleInfo());
        var vm = new ApkInstallViewModel(dsa, new ArgsText(), bundle, X64);
        await vm.LoadAsync();
        Assert.Equal(ApkInstallPage.Details, vm.Page);
        // The app is what the base APK says it is.
        Assert.Equal("Notes", vm.Title);
        Assert.Equal("com.example.notes", vm.PackageValue);
        Assert.Equal("Version: 4.2", vm.VersionLine);
        // What goes to Android: the size of everything together, and how many APKs.
        Assert.Equal("10 MB", vm.SizeChip);
        Assert.Contains(vm.Facts, f => f.Name.StartsWith("ApkWindow_FactSize") && f.Value == "10 MB");
        Assert.Contains(vm.Facts, f => f.Name.StartsWith("ApkWindow_FactApks") && f.Value == "ApkWindow_ApksValue 3|2");
        Assert.Contains(vm.Facts, f => f.Name.StartsWith("ApkWindow_FactObbs") && f.Value == "main.42.com.example.notes.obb");
        Assert.StartsWith("ApkWindow_BundleTitle", vm.SummaryTitle);
        Assert.Equal("ApkWindow_BundleSummaryObb Notes|3|10 MB", vm.Summary);
        Assert.True(vm.CanInstall);
    }

    [Theory]
    [InlineData("nowhere/app.aab", "dsa: not installed (app-bundle): app.aab: this is an Android App Bundle", "app.aab is an Android App Bundle.")]
    [InlineData("nowhere/app.apkm", "dsa: not installed (encrypted-bundle): app.apkm: the bundle is encrypted", "app.apkm is an encrypted bundle")]
    [InlineData("nowhere/app.xapk", "dsa: not installed (split-signature-mismatch): app.xapk: config.ko.apk is signed with another certificate", "app.xapk holds a split APK signed with another certificate")]
    [InlineData("nowhere/app.apks", "dsa: not installed (invalid-bundle): app.apks: the bundle holds two base APKs, a.apk and b.apk", "app.apks is a split APK bundle that cannot be installed: the bundle holds two base APKs")]
    public async Task Fr11_a_bundle_dsa_refuses_says_its_reason(string path, string error, string start)
    {
        var dsa = new FakeRunner().Fail($"apk-info {path} --json", error);
        var vm = new ApkInstallViewModel(dsa, new ArgsText(), path, X64);
        await vm.LoadAsync();
        Assert.Equal(ApkInstallPage.Failed, vm.Page);
        Assert.StartsWith("ApkWindow_NotApkTitle", vm.ResultTitle);
        Assert.StartsWith(start, vm.ResultText);
        Assert.DoesNotContain("not supported", vm.ResultText);
        Assert.False(vm.CanInstall);
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
    public async Task Fr25_an_arm64_only_app_refused_while_translation_is_off_says_so()
    {
        var dsa = new FakeRunner()
            .Answer($"apk-info {Path} --json", Info("running", "\"new\""))
            .Fail($"install {Path} --progress --json",
                "dsa: not installed (translation-off): notes.apk: the app's native code is for arm64-v8a only, which DSA's Android runs through ARM translation, and ARM translation is not on in the running Android. Turn it on under ARM apps, restart Android and install the app again");
        var vm = await Loaded(dsa);
        await vm.InstallAsync();
        Assert.Equal(ApkInstallPage.Failed, vm.Page);
        Assert.Equal("ApkWindow_Reason_translation-off", vm.ResultText);
    }

    [Fact]
    public async Task Fr11_a_file_that_is_not_an_apk_says_why()
    {
        var dsa = new FakeRunner().Fail($"apk-info {Path} --json", "dsa: the file is not a ZIP archive, so it is not an APK");
        var vm = await Loaded(dsa);
        Assert.Equal(ApkInstallPage.Failed, vm.Page);
        Assert.StartsWith("ApkWindow_NotApkTitle", vm.ResultTitle);
        // What dsa says is for developers; the user reads what the file is, in plain words.
        Assert.Equal("notes.apk is not a valid APK file.", vm.ResultText);
        Assert.DoesNotContain("ZIP", vm.ResultText);
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
    public void Fr11_the_installer_opens_on_reading_the_file_with_launch_when_ready_on()
    {
        var vm = new ApkInstallViewModel(new FakeRunner(), new ArgsText(), Path, X64);
        Assert.Equal(ApkInstallPage.Reading, vm.Page);
        Assert.Equal("notes.apk", vm.Title);
        Assert.False(vm.CanInstall);
        Assert.False(vm.CanOpen);
        Assert.False(vm.IsBusy);
        // As App Installer does, the app opens once it is installed unless the user says not to.
        Assert.True(vm.LaunchWhenReady);
        Assert.False(vm.Launched);
    }

    [Fact]
    public async Task Fr11_the_installer_shows_the_publisher_and_version_under_the_app_name()
    {
        var vm = await Loaded(new FakeRunner().Answer($"apk-info {Path} --json", Info()));
        Assert.Equal("Notes", vm.Title);
        Assert.StartsWith("Publisher: ApkWindow_Signer v2|AB:CD", vm.PublisherLine);
        Assert.Equal("Version: 4.2", vm.VersionLine);
        Assert.Equal("", vm.CompatibilityWarning);
    }

    [Fact]
    public async Task Fr11_an_unsigned_apk_says_so_where_the_publisher_goes()
    {
        var info = Info().Replace("\"signer\":{\"scheme\":\"v2\",\"sha256\":\"AB:CD\"},", "");
        var vm = await Loaded(new FakeRunner().Answer($"apk-info {Path} --json", info));
        Assert.StartsWith("Publisher: ApkWindow_Unsigned", vm.PublisherLine);
    }

    [Fact]
    public async Task Fr11_an_apk_without_code_for_this_processor_is_flagged_before_install()
    {
        var info = Info().Replace("\"abis\":[\"x86_64\"]", "\"abis\":[\"arm64-v8a\",\"armeabi-v7a\"]");
        var vm = await Loaded(new FakeRunner().Answer($"apk-info {Path} --json", info));
        Assert.StartsWith("ApkWindow_AbiWarning arm64-v8a, armeabi-v7a", vm.CompatibilityWarning);
        Assert.StartsWith("ApkWindow_AbiWarningTitle", vm.CompatibilityTitle);
        // Android decides with its translation package, so the install stays possible.
        Assert.True(vm.CanInstall);

        var arm = new ApkInstallViewModel(new FakeRunner().Answer($"apk-info {Path} --json", info), new ArgsText(), Path,
            new[] { "arm64-v8a" });
        await arm.LoadAsync();
        Assert.Equal("", arm.CompatibilityWarning);
    }

    /// <summary>What dsa apk-info says of an APK with native code for <paramref name="abis"/>
    /// only, and of whether Android runs arm64-v8a code (null: dsa could not say).</summary>
    private static string ArmInfo(string abis, string runsArm64) =>
        Info().Replace("\"abis\":[\"x86_64\"]", $"\"abis\":[{abis}]")
            .Replace("\"android\":", $"\"runs_arm64\":{runsArm64},\"android\":");

    [Fact]
    public async Task Fr25_an_arm64_app_is_not_flagged_while_an_arm_translator_runs_it()
    {
        // What APKPure 3.20 is: arm64-v8a code only. With the default translator on, Android
        // runs it, so the installer has nothing to warn of.
        var vm = await Loaded(new FakeRunner().Answer($"apk-info {Path} --json", ArmInfo("\"arm64-v8a\"", "true")));
        Assert.True(vm.Apk!.RunsArm64);
        Assert.Equal("", vm.CompatibilityWarning);
        Assert.Equal("", vm.CompatibilityTitle);
        Assert.True(vm.CanInstall);
        // An app with 32-bit ARM code beside it runs its arm64-v8a code.
        var both = await Loaded(new FakeRunner().Answer($"apk-info {Path} --json",
            ArmInfo("\"arm64-v8a\",\"armeabi-v7a\"", "true")));
        Assert.Equal("", both.CompatibilityWarning);
    }

    [Fact]
    public async Task Fr25_an_arm_app_is_flagged_without_a_translator_or_with_only_32_bit_code()
    {
        // No translator: the translation is off, or the image has none.
        var off = await Loaded(new FakeRunner().Answer($"apk-info {Path} --json", ArmInfo("\"arm64-v8a\"", "false")));
        Assert.False(off.Apk!.RunsArm64);
        Assert.StartsWith("ApkWindow_AbiWarning arm64-v8a", off.CompatibilityWarning);
        Assert.StartsWith("ApkWindow_AbiWarningTitle", off.CompatibilityTitle);
        // dsa could not ask the service: the warning stays, as before it could.
        var unknown = await Loaded(new FakeRunner().Answer($"apk-info {Path} --json", ArmInfo("\"arm64-v8a\"", "null")));
        Assert.Null(unknown.Apk!.RunsArm64);
        Assert.StartsWith("ApkWindow_AbiWarning arm64-v8a", unknown.CompatibilityWarning);
        // The translator runs no 32-bit ARM code, so an app with only that is flagged with it on,
        // and told why.
        var old = await Loaded(new FakeRunner().Answer($"apk-info {Path} --json",
            ArmInfo("\"armeabi-v7a\",\"armeabi\"", "true")));
        Assert.StartsWith("ApkWindow_Abi32Warning armeabi-v7a, armeabi", old.CompatibilityWarning);
        Assert.StartsWith("ApkWindow_AbiWarningTitle", old.CompatibilityTitle);
        var oldOff = await Loaded(new FakeRunner().Answer($"apk-info {Path} --json",
            ArmInfo("\"armeabi-v7a\"", "false")));
        Assert.StartsWith("ApkWindow_AbiWarning armeabi-v7a", oldOff.CompatibilityWarning);
        // Android decides either way, so the install stays possible.
        Assert.True(old.CanInstall);
        Assert.True(off.CanInstall);
    }

    [Fact]
    public async Task Fr11_launch_when_ready_opens_the_app_once_it_is_installed()
    {
        var dsa = new FakeRunner()
            .Answer($"apk-info {Path} --json", Info("running", "\"new\""))
            .Answer($"install {Path} --progress --json", "{\"installed\":true}", "stage sending", "progress 100")
            .Answer("launch com.example.notes", "Launching com.example.notes");
        var vm = await Loaded(dsa, launchWhenReady: true);
        await vm.InstallAsync();
        Assert.Equal(ApkInstallPage.Done, vm.Page);
        Assert.True(vm.Launched);
        Assert.Equal("launch com.example.notes", dsa.Calls[^1]);
    }

    [Fact]
    public async Task Fr11_without_launch_when_ready_the_done_screen_offers_launch()
    {
        var dsa = new FakeRunner()
            .Answer($"apk-info {Path} --json", Info("running", "\"new\""))
            .Answer($"install {Path} --progress --json", "{\"installed\":true}");
        var vm = await Loaded(dsa);
        await vm.InstallAsync();
        Assert.Equal(ApkInstallPage.Done, vm.Page);
        Assert.False(vm.Launched);
        Assert.True(vm.CanOpen);
        Assert.DoesNotContain(dsa.Calls, c => c.StartsWith("launch"));
    }

    [Fact]
    public async Task Fr11_an_app_that_does_not_open_after_install_stays_on_done_with_the_reason()
    {
        var dsa = new FakeRunner()
            .Answer($"apk-info {Path} --json", Info("running", "\"new\""))
            .Answer($"install {Path} --progress --json", "{\"installed\":true}")
            .Fail("launch com.example.notes", "dsa: the window manager is not running");
        var vm = await Loaded(dsa, launchWhenReady: true);
        await vm.InstallAsync();
        Assert.Equal(ApkInstallPage.Done, vm.Page);
        Assert.False(vm.Launched);
        Assert.True(vm.CanOpen);
        Assert.StartsWith("ApkWindow_LaunchFailedAfterInstall Notes|the window manager is not running", vm.ResultText);
    }

    [Fact]
    public async Task Fr11_the_progress_state_can_be_cancelled_until_android_has_the_app()
    {
        var dsa = new FakeRunner()
            .Answer($"apk-info {Path} --json", Info("running", "\"new\""))
            .Answer($"install {Path} --progress --json", "{\"installed\":true}", "stage sending", "progress 30", "stage installing");
        var vm = await Loaded(dsa);
        var seen = new List<(ApkInstallPage Page, bool Busy, bool CanCancel)>();
        vm.PropertyChanged += (_, _) => seen.Add((vm.Page, vm.IsBusy, vm.CanCancel));
        await vm.InstallAsync();
        Assert.Contains((ApkInstallPage.Progress, true, true), seen);
        // Once Android has the whole APK, the install cannot be stopped.
        Assert.Contains((ApkInstallPage.Progress, true, false), seen);
        Assert.False(vm.IsBusy);
    }

    [Fact]
    public async Task Fr11_the_labels_are_english_whatever_the_language()
    {
        // The test's text source answers every table lookup with the key, so a label that came
        // from a string table would not read as the English word.
        var vm = await Loaded(new FakeRunner().Answer($"apk-info {Path} --json", Info()));
        Assert.Equal("Install Notes?", vm.HeroTitle);
        Assert.Equal("Package Name: com.example.notes", vm.PackageLine);
        Assert.StartsWith("Publisher: ", vm.PublisherLine);
        Assert.Equal("Version: 4.2", vm.VersionLine);
        Assert.StartsWith("Source: ", vm.SourceLine);
        Assert.Equal("Capabilities (2):", vm.PermissionsHeader);
        Assert.Equal("More", vm.MoreLabel);
    }

    [Fact]
    public async Task Fr11_a_plain_install_adds_no_sentence()
    {
        var vm = await Loaded(new FakeRunner().Answer($"apk-info {Path} --json", Info("running", "\"new\"")));
        Assert.Equal("", vm.Summary);
        Assert.Equal("", vm.SummaryTitle);
    }

    [Theory]
    [InlineData("\"update\"", "ApkWindow_InstalledTitle")]
    [InlineData("\"reinstall\"", "ApkWindow_InstalledTitle")]
    [InlineData("\"downgrade\"", "ApkWindow_DowngradeTitle")]
    [InlineData("\"signature-mismatch\"", "ApkWindow_SignatureTitle")]
    public async Task Fr11_every_notice_has_a_short_title(string kind, string title)
    {
        var vm = await Loaded(new FakeRunner().Answer($"apk-info {Path} --json", Info("running", kind, Installed41)));
        Assert.StartsWith(title, vm.SummaryTitle);
        Assert.NotEqual("", vm.Summary);
    }

    [Fact]
    public async Task Fr11_more_expands_from_the_first_three_permissions_to_all_of_them()
    {
        var permissions = string.Join(",", Enumerable.Range(1, 40).Select(i =>
            $"{{\"name\":\"android.permission.P{i}\",\"description\":\"Permission {i}\",\"sensitive\":false}}"));
        var info = Regex.Replace(Info(), "\"permissions\":\\[.*?\\],\"icon\"", $"\"permissions\":[{permissions}],\"icon\"");
        var vm = await Loaded(new FakeRunner().Answer($"apk-info {Path} --json", info));
        Assert.Equal("Capabilities (40):", vm.PermissionsHeader);
        Assert.False(vm.IsExpanded);
        Assert.Equal(new[] { "Permission 1", "Permission 2", "Permission 3" }, vm.PermissionPreview.Select(p => p.Description));

        var changed = false;
        vm.PropertyChanged += (_, _) => changed = true;
        vm.Expand();
        Assert.True(vm.IsExpanded);
        Assert.True(changed);
        Assert.Equal(40, vm.Permissions.Count);
        // The raw names stay, for the expanded rows.
        Assert.Equal("android.permission.P40", vm.Permissions[39].Name);
    }

    [Fact]
    public async Task Fr11_more_turns_into_less_and_less_folds_the_list_again()
    {
        var vm = await Loaded(new FakeRunner().Answer($"apk-info {Path} --json", Info()));
        Assert.False(vm.IsExpanded);
        Assert.Equal("More", vm.MoreLabel);

        vm.ToggleExpanded();
        Assert.True(vm.IsExpanded);
        Assert.Equal("Less", vm.MoreLabel);

        var changed = false;
        vm.PropertyChanged += (_, _) => changed = true;
        vm.ToggleExpanded();
        Assert.True(changed);
        Assert.False(vm.IsExpanded);
        Assert.Equal("More", vm.MoreLabel);

        vm.Expand();
        vm.Collapse();
        Assert.False(vm.IsExpanded);
    }

    [Fact]
    public async Task Fr11_the_lines_under_the_title_have_their_labels_in_one_column_and_values_in_the_next()
    {
        var vm = await Loaded(new FakeRunner().Answer($"apk-info {Path} --json", Info()));
        Assert.Equal("Package Name:", ApkInstallViewModel.PackageLabel);
        Assert.Equal("Publisher:", ApkInstallViewModel.PublisherLabel);
        Assert.Equal("Version:", ApkInstallViewModel.VersionLabel);
        Assert.Equal("Source:", ApkInstallViewModel.SourceLabel);
        Assert.Equal("com.example.notes", vm.PackageValue);
        Assert.Equal("4.2", vm.VersionValue);
        Assert.Equal("Package Name: com.example.notes", vm.PackageLine);
    }

    [Theory]
    [InlineData("nowhere/app.xapk", "app.xapk is a split APK bundle")]
    [InlineData("nowhere/app.apks", "app.apks is a split APK bundle")]
    [InlineData("nowhere/app.apkm", "app.apkm is a split APK bundle")]
    [InlineData("nowhere/app.aab", "app.aab is an Android App Bundle")]
    [InlineData("nowhere/app.apk", "app.apk is not a valid APK file.")]
    public async Task Fr11_a_file_that_cannot_be_installed_is_named_for_what_it_is(string path, string start)
    {
        var dsa = new FakeRunner().Fail($"apk-info {path} --json", "dsa: the file has no AndroidManifest.xml, so it is not an APK");
        var vm = new ApkInstallViewModel(dsa, new ArgsText(), path, X64);
        await vm.LoadAsync();
        Assert.Equal(ApkInstallPage.Failed, vm.Page);
        Assert.StartsWith("ApkWindow_NotApkTitle", vm.ResultTitle);
        Assert.StartsWith(start, vm.ResultText);
        Assert.DoesNotContain("AndroidManifest", vm.ResultText);
    }

    [Fact]
    public async Task Fr11_reading_the_file_again_never_takes_its_icon_away()
    {
        var withoutIcon = Regex.Replace(Info("running", "\"update\"", Installed41), "\"icon\":\\{.*?\\}\\},\"signer\"", "\"icon\":null,\"signer\"");
        Assert.Contains("\"icon\":null", withoutIcon);
        var dsa = new FakeRunner()
            .Answer($"apk-info {Path} --json", Info())
            .Answer("start --wait --json", "{\"state\":\"Active\"}")
            .Answer($"apk-info {Path} --json", withoutIcon);
        var vm = await Loaded(dsa);
        var icon = vm.Apk!.Icon;
        Assert.NotNull(icon);
        await vm.InstallAsync();
        Assert.Equal(ApkInstallKind.Update, vm.Kind);
        Assert.Same(icon, vm.Apk!.Icon);
    }

    [Fact]
    public async Task Fr11_the_title_asks_only_while_the_install_is_undecided_or_running()
    {
        var unread = new ApkInstallViewModel(new FakeRunner(), new ArgsText(), Path, X64);
        Assert.Equal("notes.apk", unread.HeroTitle);
        var dsa = new FakeRunner()
            .Answer($"apk-info {Path} --json", Info("running", "\"new\""))
            .Answer($"install {Path} --progress --json", "{\"installed\":true}");
        var vm = await Loaded(dsa);
        Assert.Equal("Install Notes?", vm.HeroTitle);
        await vm.InstallAsync();
        Assert.Equal(ApkInstallPage.Done, vm.Page);
        Assert.Equal("Notes", vm.HeroTitle);
    }

    [Theory]
    [InlineData("[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://apkpure.net/page\r\nHostUrl=https://d.example.com/a.apk\r\n", "d.example.com")]
    [InlineData("[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://apkpure.net/page\r\n", "apkpure.net")]
    // A browser's about:internet has no host, so the next value speaks.
    [InlineData("[ZoneTransfer]\r\nHostUrl=about:internet\r\nReferrerUrl=https://apkpure.net/page\r\n", "apkpure.net")]
    public void Fr11_source_is_the_host_the_file_came_from(string zone, string host)
    {
        Assert.Equal(host, ApkSource.Describe(Path, zone));
    }

    [Theory]
    [InlineData(null)]
    [InlineData("[ZoneTransfer]\r\nZoneId=3\r\n")]
    [InlineData("[ZoneTransfer]\r\nHostUrl=about:internet\r\n")]
    public void Fr11_source_without_a_host_is_the_files_folder(string? zone)
    {
        Assert.Equal(System.IO.Path.GetDirectoryName(System.IO.Path.GetFullPath(Path)), ApkSource.Describe(Path, zone));
    }

    [Fact]
    public void Fr11_source_reads_the_files_zone_identifier_stream()
    {
        if (!OperatingSystem.IsWindows())
        {
            return;
        }
        var folder = Directory.CreateTempSubdirectory("dsa-apk-source").FullName;
        try
        {
            var apk = System.IO.Path.Combine(folder, "app.apk");
            File.WriteAllText(apk, "x");
            Assert.Equal(folder, ApkSource.Of(apk));
            File.WriteAllText(apk + ":Zone.Identifier", "[ZoneTransfer]\r\nHostUrl=https://d.example.com/app.apk\r\n");
            Assert.Equal("d.example.com", ApkSource.Of(apk));
        }
        finally
        {
            Directory.Delete(folder, true);
        }
    }

    [Fact]
    public void Fr11_sizes_read_in_the_users_units()
    {
        Assert.Equal("512 KB", ApkInstallViewModel.Size(512 * 1024));
        Assert.Equal("1.5 MB", ApkInstallViewModel.Size(3 * 512 * 1024));
        Assert.Equal("2 GB", ApkInstallViewModel.Size(2L * 1024 * 1024 * 1024));
    }
}
