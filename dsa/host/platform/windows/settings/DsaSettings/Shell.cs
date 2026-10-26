using System.Diagnostics;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Dsa.Settings;

/// <summary>What the app does on this machine itself: opening folders and restarting the
/// user's window manager. None of it changes a setting.</summary>
internal static class Shell
{
    /// <summary>This user's DSA logs: the window manager's and the app windows'.</summary>
    public static string UserLogFolder => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "DSA", "logs");

    public static string LicenseFolder => Path.Combine(App.InstallFolder, "licenses");

    public static async void OpenFolder(string path, XamlRoot root)
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
                await Tell(root, App.Text.Get("FolderMissingTitle"), $"{path}\n{e.Message}");
                return;
            }
        }
        await Tell(root, App.Text.Get("FolderMissingTitle"), string.Format(App.Text.Get("FolderMissingBody"), path));
    }

    public static async Task Tell(XamlRoot root, string title, string message)
    {
        var dialog = new ContentDialog
        {
            XamlRoot = root,
            Title = title,
            Content = new TextBlock { Text = message, TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true },
            CloseButtonText = App.Text.Get("OkButton"),
        };
        await dialog.ShowAsync();
    }

    public static async Task<bool> Confirm(XamlRoot root, string title, string message, string action)
    {
        var dialog = new ContentDialog
        {
            XamlRoot = root,
            Title = title,
            Content = new TextBlock { Text = message, TextWrapping = TextWrapping.Wrap },
            PrimaryButtonText = action,
            CloseButtonText = App.Text.Get("CancelButton"),
            DefaultButton = ContentDialogButton.Close,
        };
        return await dialog.ShowAsync() == ContentDialogResult.Primary;
    }

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
