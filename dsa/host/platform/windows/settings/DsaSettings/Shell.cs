using System.Diagnostics;
using MessageBox = Wpf.Ui.Controls.MessageBox;
using MessageBoxResult = Wpf.Ui.Controls.MessageBoxResult;

namespace Dsa.Settings;

/// <summary>What the app does on this machine itself: opening folders, asking the user, and
/// restarting the user's window manager. None of it changes a setting.</summary>
internal static class Shell
{
    /// <summary>This user's DSA logs: the window manager's and the app windows'.</summary>
    public static string UserLogFolder => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "DSA", "logs");

    public static string LicenseFolder => Path.Combine(App.InstallFolder, "licenses");

    public static async void OpenFolder(string path)
    {
        if (!string.IsNullOrEmpty(path) && Directory.Exists(path))
        {
            try
            {
                Process.Start(new ProcessStartInfo("explorer.exe") { ArgumentList = { path }, UseShellExecute = false });
                return;
            }
            catch (Exception e) when (e is System.ComponentModel.Win32Exception or InvalidOperationException)
            {
                await Tell(App.Text.Get("FolderMissingTitle"), $"{path}\n{e.Message}");
                return;
            }
        }
        await Tell(App.Text.Get("FolderMissingTitle"), string.Format(App.Text.Get("FolderMissingBody"), path));
    }

    public static async Task Tell(string title, string message)
    {
        var box = new MessageBox
        {
            Title = title,
            Content = new System.Windows.Controls.TextBlock { Text = message, TextWrapping = System.Windows.TextWrapping.Wrap },
            CloseButtonText = App.Text.Get("OkButton"),
        };
        await box.ShowDialogAsync();
    }

    public static async Task<bool> Confirm(string title, string message, string action)
    {
        var box = new MessageBox
        {
            Title = title,
            Content = new System.Windows.Controls.TextBlock { Text = message, TextWrapping = System.Windows.TextWrapping.Wrap },
            PrimaryButtonText = action,
            CloseButtonText = App.Text.Get("CancelButton"),
        };
        return await box.ShowDialogAsync() == MessageBoxResult.Primary;
    }

    /// <summary>Whether this app runs as an administrator (elevated).</summary>
    public static bool IsAdministrator()
    {
        using var identity = System.Security.Principal.WindowsIdentity.GetCurrent();
        return new System.Security.Principal.WindowsPrincipal(identity)
            .IsInRole(System.Security.Principal.WindowsBuiltInRole.Administrator);
    }

    /// <summary>Runs the dsa command with <paramref name="args"/> as an administrator: Windows
    /// asks the user to allow it. Returns an error to show, or null.</summary>
    public static async Task<string?> RunDsaElevated(params string[] args)
    {
        var start = new ProcessStartInfo(ProcessDsaCliPath)
        {
            UseShellExecute = true,
            Verb = "runas",
            WindowStyle = ProcessWindowStyle.Hidden,
        };
        foreach (var arg in args)
        {
            start.ArgumentList.Add(arg);
        }
        try
        {
            using var process = Process.Start(start);
            if (process is null)
            {
                return App.Text.Get("Problem_CliMissing");
            }
            await process.WaitForExitAsync();
            return process.ExitCode == 0
                ? null
                : string.Format(App.Text.Get("ElevatedFailed"), process.ExitCode);
        }
        catch (System.ComponentModel.Win32Exception e) when (e.NativeErrorCode == 1223)
        {
            // The user declined the prompt.
            return App.Text.Get("ElevationDeclined");
        }
        catch (Exception e) when (e is System.ComponentModel.Win32Exception or InvalidOperationException)
        {
            return e.Message;
        }
    }

    private static string ProcessDsaCliPath => Dsa.Settings.Core.ProcessDsaCli.Locate(AppContext.BaseDirectory);

    /// <summary>Ends this user's window manager in this session and starts it again, so it
    /// chooses its render GPU anew. Returns an error to show, or null.</summary>
    public static async Task<string?> RestartWindowManager()
    {
        var program = Path.Combine(App.InstallFolder, "dsa-wm.exe");
        if (!File.Exists(program))
        {
            return string.Format(App.Text.Get("FolderMissingBody"), program);
        }
        var session = Process.GetCurrentProcess().SessionId;
        try
        {
            foreach (var running in Process.GetProcessesByName("dsa-wm"))
            {
                using (running)
                {
                    if (running.SessionId != session)
                    {
                        continue;
                    }
                    running.Kill();
                    await running.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(10));
                }
            }
            Process.Start(new ProcessStartInfo(program) { UseShellExecute = false, WorkingDirectory = App.InstallFolder });
            return null;
        }
        catch (Exception e)
        {
            return e.Message;
        }
    }
}
