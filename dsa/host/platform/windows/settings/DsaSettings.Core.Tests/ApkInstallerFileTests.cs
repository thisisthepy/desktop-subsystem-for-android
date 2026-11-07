using System.IO.Compression;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>What the APK Installer makes of the file it is given: a bundle named for what it
/// is, and an icon drawn whatever it is made of.</summary>
public class ApkInstallerFileTests
{
    private static string Zip(string folder, string name, params string[] entries)
    {
        var path = Path.Combine(folder, name);
        using var zip = ZipFile.Open(path, ZipArchiveMode.Create);
        foreach (var entry in entries)
        {
            using var writer = new StreamWriter(zip.CreateEntry(entry).Open());
            writer.Write("x");
        }
        return path;
    }

    [Theory]
    // Named .apk, as a download often is: what is inside says what it is.
    [InlineData(UnsupportedKind.SplitBundle, "manifest.json", "base.apk", "config.arm64_v8a.apk")]
    [InlineData(UnsupportedKind.SplitBundle, "toc.pb", "splits/base-master.apk")]
    [InlineData(UnsupportedKind.SplitBundle, "info.json", "base.apk")]
    [InlineData(UnsupportedKind.AppBundle, "BundleConfig.pb", "base/manifest/AndroidManifest.xml")]
    [InlineData(UnsupportedKind.AppBundle, "base/manifest/AndroidManifest.xml", "base/dex/classes.dex")]
    [InlineData(UnsupportedKind.NotApk, "readme.txt")]
    public void Fr11_a_bundle_is_known_by_what_is_in_it(UnsupportedKind kind, params string[] entries)
    {
        var folder = Directory.CreateTempSubdirectory("dsa-apk-bundle").FullName;
        try
        {
            Assert.Equal(kind, UnsupportedFile.Classify(Zip(folder, "download.apk", entries)));
        }
        finally
        {
            Directory.Delete(folder, true);
        }
    }

    [Fact]
    public void Fr11_a_file_that_is_no_archive_is_known_by_its_extension()
    {
        var folder = Directory.CreateTempSubdirectory("dsa-apk-bundle").FullName;
        try
        {
            foreach (var (name, kind) in new[] { ("a.xapk", UnsupportedKind.SplitBundle), ("a.apks", UnsupportedKind.SplitBundle),
                ("a.apkm", UnsupportedKind.SplitBundle), ("a.aab", UnsupportedKind.AppBundle), ("a.apk", UnsupportedKind.NotApk) })
            {
                var path = Path.Combine(folder, name);
                File.WriteAllText(path, "not a zip");
                Assert.Equal(kind, UnsupportedFile.Classify(path));
            }
        }
        finally
        {
            Directory.Delete(folder, true);
        }
    }

    [Fact]
    public void Fr11_each_kind_of_file_has_its_plain_sentence()
    {
        // Bundles install now; one is refused only for what is wrong with it.
        Assert.Equal("a.xapk is a split APK bundle that cannot be read, so it cannot be installed.",
            UnsupportedFile.Message(UnsupportedKind.SplitBundle, "a.xapk"));
        Assert.Equal("a.xapk is a split APK bundle that cannot be installed: the bundle holds no base APK.",
            UnsupportedFile.Message(UnsupportedKind.SplitBundle, "a.xapk", "the bundle holds no base APK"));
        foreach (var kind in Enum.GetValues<UnsupportedKind>())
        {
            Assert.DoesNotContain("not supported", UnsupportedFile.Message(kind, "a.xapk"));
        }
        var aab = UnsupportedFile.Message(UnsupportedKind.AppBundle, "a.aab");
        Assert.StartsWith("a.aab is an Android App Bundle.", aab);
        Assert.Contains("another key", aab);
        Assert.Contains("Get the app as an APK", aab);
        Assert.StartsWith("a.apkm is an encrypted bundle", UnsupportedFile.Message(UnsupportedKind.EncryptedBundle, "a.apkm"));
        Assert.Contains("another certificate", UnsupportedFile.Message(UnsupportedKind.SplitSignatureMismatch, "a.xapk"));
        Assert.Equal("a.bin is not a valid APK file.", UnsupportedFile.Message(UnsupportedKind.NotApk, "a.bin"));
    }

    [Theory]
    [InlineData("dsa: not installed (app-bundle): a.aab: this is an Android App Bundle", UnsupportedKind.AppBundle)]
    [InlineData("dsa: not installed (encrypted-bundle): a.apkm: the bundle is encrypted", UnsupportedKind.EncryptedBundle)]
    [InlineData("dsa: not installed (split-signature-mismatch): a.xapk: config.ko.apk is signed with another certificate", UnsupportedKind.SplitSignatureMismatch)]
    [InlineData("dsa: not installed (invalid-bundle): a.apks: the bundle holds two base APKs", UnsupportedKind.SplitBundle)]
    public void Fr11_a_bundle_dsa_refuses_is_named_by_its_reason(string error, UnsupportedKind kind)
    {
        var refusal = UnsupportedFile.FromRefusal(error);
        Assert.NotNull(refusal);
        Assert.Equal(kind, refusal!.Value.Kind);
        Assert.Null(UnsupportedFile.FromRefusal("dsa: the file is not a ZIP archive, so it is not an APK"));
        Assert.Null(UnsupportedFile.FromRefusal("dsa: not installed (incompatible): a.apk: The app does not run"));
    }

    [Fact]
    public void Fr11_a_bundle_reads_as_its_base_with_what_installing_it_sends()
    {
        var details = ApkDetails.Parse(Head + "\"icon\":null,\"signer\":null,\"abis\":[\"arm64-v8a\",\"x86_64\"]," +
            "\"android\":\"stopped\",\"installed\":null,\"install\":null,\"format\":\"xapk\"," +
            "\"apks\":[\"base.apk\",\"split_config.x86_64.apk\",\"split_config.en.apk\"],\"split_count\":2," +
            "\"obbs\":[\"main.1.com.example.vector.obb\"],\"total_size\":3000,\"install_abi\":\"x86_64\"}");
        Assert.True(details.IsBundle);
        Assert.Equal(3, details.Apks!.Count);
        Assert.Equal(2, details.SplitCount);
        Assert.Equal("main.1.com.example.vector.obb", Assert.Single(details.Obbs!));
        Assert.Equal(3000, details.InstallSize);
        var apk = ApkDetails.Parse(Head + "\"icon\":null,\"signer\":null,\"abis\":[],\"android\":\"stopped\"," +
            "\"installed\":null,\"install\":null}");
        Assert.False(apk.IsBundle);
        Assert.Equal(1, apk.InstallSize);
    }

    private const string Head =
        "{\"package\":\"com.example.vector\",\"version_code\":1,\"min_sdk\":26,\"target_sdk\":34,\"size\":1,\"permissions\":[],";

    [Fact]
    public void Fr11_an_adaptive_icon_with_a_vector_foreground_keeps_its_foreground()
    {
        // Chrome's icon: a colour behind a vector drawable. The foreground was dropped, so the
        // tile showed a plain square.
        var details = ApkDetails.Parse(Head +
            "\"icon\":{\"kind\":\"adaptive\",\"background\":{\"kind\":\"color\",\"color\":\"#FFFFFFFF\"}," +
            "\"foreground\":{\"kind\":\"vector\",\"width\":108.0,\"height\":108.0,\"inset\":0.0,\"paths\":[" +
            "{\"data\":\"M0,0L10,0L10,10z\",\"fill\":\"#FFDB4437\",\"stroke\":null,\"stroke_width\":0.0,\"even_odd\":true," +
            "\"transform\":[1.0,0.0,0.0,1.0,30.0,30.0]}]}}," +
            "\"signer\":null,\"abis\":[],\"android\":\"stopped\",\"installed\":null,\"install\":null}");
        var icon = details.Icon!;
        Assert.True(icon.IsAdaptive);
        Assert.Equal(0xFFFFFFFFu, icon.Background!.Color);
        var vector = icon.Foreground!.Vector!;
        Assert.Equal((108.0, 108.0, 0.0), (vector.Width, vector.Height, vector.Inset));
        var path = Assert.Single(vector.Paths);
        Assert.Equal("M0,0L10,0L10,10z", path.Data);
        Assert.Equal(0xFFDB4437u, path.Fill);
        Assert.Null(path.Stroke);
        Assert.True(path.EvenOdd);
        Assert.Equal(new[] { 1.0, 0, 0, 1, 30, 30 }, path.Transform);
    }

    [Fact]
    public void Fr11_a_plain_png_icon_reads_as_its_image()
    {
        var details = ApkDetails.Parse(Head + "\"icon\":{\"kind\":\"image\",\"png\":\"iVBORw==\"}," +
            "\"signer\":null,\"abis\":[],\"android\":\"stopped\",\"installed\":null,\"install\":null}");
        Assert.Equal(new byte[] { 0x89, 0x50, 0x4E, 0x47 }, details.Icon!.Png);
        Assert.False(details.Icon.IsAdaptive);
        Assert.Null(details.Icon.Vector);
    }

    [Fact]
    public void Fr11_a_vector_icon_on_its_own_reads_as_a_vector()
    {
        var details = ApkDetails.Parse(Head + "\"icon\":{\"kind\":\"vector\",\"width\":24.0,\"height\":24.0,\"inset\":0.0," +
            "\"paths\":[{\"data\":\"M0,0h24v24h-24z\",\"fill\":\"#FF000000\",\"transform\":[1,0,0,1,0,0]}]}," +
            "\"signer\":null,\"abis\":[],\"android\":\"stopped\",\"installed\":null,\"install\":null}");
        Assert.False(details.Icon!.IsAdaptive);
        Assert.Equal(24.0, details.Icon.Vector!.Width);
    }

    [Theory]
    // Android leaves out separators SVG allows to leave out; WPF needs them.
    [InlineData("M1.5.5l-2-3", "M 1.5 0.5 l -2 -3")]
    [InlineData("M24,14a10,10 0,1 1,-.01 0z", "M 24 14 a 10 10 0 1 1 -0.01 0 z")]
    [InlineData("M1e2,2E-1L3 4", "M 100 0.2 L 3 4")]
    public void Fr11_vector_path_data_is_made_readable_for_wpf(string android, string wpf)
    {
        Assert.Equal(wpf, AndroidPathData.ToWpf(android));
    }
}
