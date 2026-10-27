using System.Diagnostics;
using System.Text;

namespace Dsa.Settings.Core;

/// <summary>What one run of the dsa command printed.</summary>
public sealed record CliResult(int ExitCode, string Output, string Error);

/// <summary>Runs the dsa command. The app never speaks the local API itself: dsa does, and
/// prints its answer as JSON.</summary>
public interface IDsaCli
{
    Task<CliResult> RunAsync(IReadOnlyList<string> args, CancellationToken cancel = default);
}

/// <summary>Runs dsa.exe as a hidden child process.</summary>
public sealed class ProcessDsaCli : IDsaCli
{
    private readonly string _path;

    public ProcessDsaCli(string path)
    {
        _path = path;
    }

    /// <summary>Where dsa.exe is: DSA_CLI when it is set, then the program folder above the
    /// settings app's own folder (where the installer puts both), then the PATH.</summary>
    public static string Locate(string appFolder)
    {
        var fromEnvironment = Environment.GetEnvironmentVariable("DSA_CLI");
        if (!string.IsNullOrEmpty(fromEnvironment))
        {
            return fromEnvironment;
        }
        foreach (var candidate in new[]
                 {
                     Path.Combine(appFolder, "..", "dsa.exe"),
                     Path.Combine(appFolder, "dsa.exe"),
                 })
        {
            if (File.Exists(candidate))
            {
                return Path.GetFullPath(candidate);
            }
        }
        return "dsa.exe";
    }

    public async Task<CliResult> RunAsync(IReadOnlyList<string> args, CancellationToken cancel = default)
    {
        var start = new ProcessStartInfo(_path)
        {
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            StandardOutputEncoding = Encoding.UTF8,
            StandardErrorEncoding = Encoding.UTF8,
        };
        foreach (var arg in args)
        {
            start.ArgumentList.Add(arg);
        }
        using var process = new Process { StartInfo = start };
        try
        {
            process.Start();
        }
        catch (Exception e) when (e is System.ComponentModel.Win32Exception or InvalidOperationException)
        {
            throw new DsaException(Problem.CliMissing, $"{_path}: {e.Message}");
        }
        var output = process.StandardOutput.ReadToEndAsync(cancel);
        var error = process.StandardError.ReadToEndAsync(cancel);
        await process.WaitForExitAsync(cancel).ConfigureAwait(true);
        return new CliResult(process.ExitCode, (await output.ConfigureAwait(true)).Trim(),
            (await error.ConfigureAwait(true)).Trim());
    }
}

/// <summary>What kind of thing went wrong, so the page can say it in the user's language.
/// The detail is what dsa said, in English, for a bug report.</summary>
public enum Problem
{
    /// <summary>dsa.exe could not be started.</summary>
    CliMissing,
    /// <summary>dsa could not reach the DSA service.</summary>
    ServiceUnreachable,
    /// <summary>The service refused or failed the request; the detail says why.</summary>
    Refused,
    /// <summary>dsa printed something that is not the expected JSON.</summary>
    Unreadable,
}

public sealed class DsaException : Exception
{
    public Problem Problem { get; }

    public DsaException(Problem problem, string detail) : base(detail)
    {
        Problem = problem;
    }
}
