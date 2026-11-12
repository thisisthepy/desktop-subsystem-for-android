using System.Windows;
using System.Windows.Controls;
using Dsa.Settings.Core;

namespace Dsa.Settings.Pages;

public partial class AdvancedPage : Page
{
    private static SettingsViewModel ViewModel => App.Settings;

    public AdvancedPage()
    {
        InitializeComponent();
        DataContext = ViewModel;
        Loaded += async (_, _) => await ViewModel.LoadAsync();
    }

    private async void OnLinksToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetLinksInAndroidAsync(LinksSwitch.IsChecked == true);

    private async void OnClipboardToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetClipboardAsync(ClipboardSwitch.IsChecked == true);

    private async void OnFileSharingToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetFileSharingAsync(FileSharingSwitch.IsChecked == true);

    private async void OnFolderToggled(object sender, RoutedEventArgs e)
    {
        if (sender is CheckBox { Tag: string folder } box)
        {
            await ViewModel.SetFolderSharedAsync(folder, box.IsChecked == true);
        }
    }

    private async void OnOpenAndroidFiles(object sender, RoutedEventArgs e) =>
        await ViewModel.OpenAndroidFilesAsync();

    private async void OnArmTranslationToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetArmTranslationAsync(ArmTranslationSwitch.IsChecked == true);

    private async void OnRemoveArmTranslation(object sender, RoutedEventArgs e) =>
        await ViewModel.RemoveArmTranslationAsync();

    private async void OnUseDefaultArmTranslator(object sender, RoutedEventArgs e) =>
        await ViewModel.UseDefaultArmTranslatorAsync();

    /// <summary>Says where Google's translator comes from and under which terms, and fetches it
    /// only when the user accepts.</summary>
    private async void OnFetchGoogleArm(object sender, RoutedEventArgs e)
    {
        if (await Shell.Confirm(App.Text.Get("ArmGoogleConfirm.Title"), ViewModel.GoogleArmNotice,
                App.Text.Get("ArmGoogleConfirm.Action")))
        {
            await ViewModel.FetchGoogleArmTranslatorAsync();
        }
    }

    private void OnOpenGoogleArmTerms(object sender, RoutedEventArgs e) => OpenWebPage(ViewModel.GoogleArmTermsUrl);

    /// <summary>Says where Widevine comes from and under which terms, and takes it only when the
    /// user accepts.</summary>
    private async void OnInstallWidevine(object sender, RoutedEventArgs e)
    {
        if (await Shell.Confirm(App.Text.Get("WidevineConfirm.Title"), ViewModel.WidevineNotice,
                App.Text.Get("WidevineConfirm.Action")))
        {
            await ViewModel.InstallWidevineAsync();
        }
    }

    private async void OnRemoveWidevine(object sender, RoutedEventArgs e) =>
        await ViewModel.RemoveWidevineAsync();

    private void OnOpenWidevineTerms(object sender, RoutedEventArgs e) => OpenWebPage(ViewModel.WidevineTermsUrl);

    private async void OnInstallGapps(object sender, RoutedEventArgs e) =>
        await ViewModel.InstallGappsAsync();

    private async void OnRemoveGapps(object sender, RoutedEventArgs e) =>
        await ViewModel.RemoveGappsAsync();

    private void OnOpenGappsTerms(object sender, RoutedEventArgs e) => OpenWebPage(ViewModel.GappsTermsUrl);

    private void OnOpenGappsRegistration(object sender, RoutedEventArgs e) =>
        OpenWebPage(ViewModel.GappsRegistrationUrl);

    /// <summary>Opens an https page in the default browser. The view model gives only https
    /// addresses; anything else is left alone.</summary>
    private static void OpenWebPage(string url)
    {
        if (!url.StartsWith("https://", System.StringComparison.OrdinalIgnoreCase))
        {
            return;
        }
        try
        {
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo(url) { UseShellExecute = true });
        }
        catch (System.ComponentModel.Win32Exception)
        {
            // No program opens web pages on this computer, so nothing opens.
        }
    }

    /// <summary>Copies the Android ID the registration page asks for, and shows a check mark
    /// on the button for a moment.</summary>
    private async void OnCopyGappsDeviceId(object sender, RoutedEventArgs e)
    {
        try
        {
            System.Windows.Clipboard.SetDataObject(GappsDeviceIdBox.Text, copy: true);
        }
        catch (System.Runtime.InteropServices.ExternalException)
        {
            return;
        }
        GappsDeviceIdCopyButton.Icon = new Wpf.Ui.Controls.SymbolIcon { Symbol = Wpf.Ui.Controls.SymbolRegular.Checkmark24 };
        await System.Threading.Tasks.Task.Delay(1500);
        GappsDeviceIdCopyButton.Icon = new Wpf.Ui.Controls.SymbolIcon { Symbol = Wpf.Ui.Controls.SymbolRegular.Copy24 };
    }

    /// <summary>Copies the one dsa command whose row the button is on (its <c>Tag</c>), and
    /// shows a check mark on that button for a moment, as the adb command's button does. The
    /// clipboard can be held by another program, and then the button stays as it was.</summary>
    private async void OnCopyCommand(object sender, RoutedEventArgs e)
    {
        if (sender is not Wpf.Ui.Controls.Button button || button.Tag is not string command)
        {
            return;
        }
        try
        {
            System.Windows.Clipboard.SetDataObject(command, copy: true);
        }
        catch (System.Runtime.InteropServices.ExternalException)
        {
            return;
        }
        button.Icon = new Wpf.Ui.Controls.SymbolIcon { Symbol = Wpf.Ui.Controls.SymbolRegular.Checkmark24 };
        await System.Threading.Tasks.Task.Delay(1500);
        button.Icon = new Wpf.Ui.Controls.SymbolIcon { Symbol = Wpf.Ui.Controls.SymbolRegular.Copy24 };
    }

    /// <summary>Turns backup on, or off; off while backups are kept, asks whether they are
    /// deleted too or kept for a later restore.</summary>
    private async void OnBackupToggled(object sender, RoutedEventArgs e)
    {
        var on = BackupSwitch.IsChecked == true;
        var delete = false;
        if (!on && ViewModel.HasBackupsKept)
        {
            delete = await Shell.Ask(App.Text.Get("BackupOffConfirm.Title"), ViewModel.BackupDeleteQuestion,
                App.Text.Get("BackupOffConfirm.Delete"), App.Text.Get("BackupOffConfirm.Keep"));
        }
        await ViewModel.SetBackupAsync(on, delete);
    }

    private async void OnBackUpNow(object sender, RoutedEventArgs e) => await ViewModel.BackUpNowAsync();

    /// <summary>Asks first, saying each app's data is replaced by the backup's, then restores.</summary>
    private async void OnRestoreBackup(object sender, RoutedEventArgs e)
    {
        if (await Shell.Confirm(App.Text.Get("BackupRestoreConfirm.Title"), ViewModel.RestoreWarning,
                App.Text.Get("BackupRestoreConfirm.Action")))
        {
            await ViewModel.RestoreBackupAsync();
        }
    }

    private async void OnShowRecoveryKey(object sender, RoutedEventArgs e) => await ViewModel.ShowRecoveryKeyAsync();

    private void OnHideRecoveryKey(object sender, RoutedEventArgs e) => ViewModel.HideRecoveryKey();

    /// <summary>Copies the recovery key, and shows a check mark on the button for a moment.</summary>
    private async void OnCopyRecoveryKey(object sender, RoutedEventArgs e)
    {
        try
        {
            System.Windows.Clipboard.SetDataObject(RecoveryKeyBox.Text, copy: true);
        }
        catch (System.Runtime.InteropServices.ExternalException)
        {
            return;
        }
        RecoveryKeyCopyButton.Icon = new Wpf.Ui.Controls.SymbolIcon { Symbol = Wpf.Ui.Controls.SymbolRegular.Checkmark24 };
        await System.Threading.Tasks.Task.Delay(1500);
        RecoveryKeyCopyButton.Icon = new Wpf.Ui.Controls.SymbolIcon { Symbol = Wpf.Ui.Controls.SymbolRegular.Copy24 };
    }

    /// <summary>Keeps the recovery key the user typed as their backup key; the box is emptied
    /// once it is kept.</summary>
    private async void OnUseRecoveryKey(object sender, RoutedEventArgs e)
    {
        await ViewModel.UseRecoveryKeyAsync(RecoveryKeyEntry.Text ?? "");
        if (!ViewModel.HasBackupError)
        {
            RecoveryKeyEntry.Text = "";
        }
    }

    /// <summary>Asks first, saying what is lost, then resets Android's data.</summary>
    private async void OnResetData(object sender, RoutedEventArgs e)
    {
        if (await Shell.Confirm(App.Text.Get("ResetDataConfirm.Title"), ViewModel.ResetWarning,
                App.Text.Get("ResetDataConfirm.Action")))
        {
            await ViewModel.ResetDataAsync();
        }
    }

    private async void OnFreezeToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetFreezeAsync(FreezeSwitch.IsChecked == true);

    private async void OnInputTraceToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetInputTraceAsync(InputTraceSwitch.IsChecked == true);

    private async void OnResizeTraceToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetResizeTraceAsync(ResizeTraceSwitch.IsChecked == true);

    private async void OnRootElevationToggled(object sender, RoutedEventArgs e) =>
        await ViewModel.SetRootElevationAsync(RootElevationSwitch.IsChecked == true);

    private void OnRootElevationAsAdministrator(object sender, RoutedEventArgs e) =>
        ChangeAsAdministrator("root-elevation", !ViewModel.RootElevation);

    private void OnInputTraceAsAdministrator(object sender, RoutedEventArgs e) =>
        ChangeAsAdministrator("input-trace", !ViewModel.InputTrace);

    private void OnResizeTraceAsAdministrator(object sender, RoutedEventArgs e) =>
        ChangeAsAdministrator("resize-trace", !ViewModel.ResizeTrace);

    /// <summary>Runs <c>dsa settings set</c> as an administrator (Windows asks the user),
    /// then shows what the service keeps.</summary>
    private static async void ChangeAsAdministrator(string name, bool on)
    {
        var problem = await Shell.RunDsaElevated("settings", "set", name, on ? "on" : "off");
        if (problem is not null)
        {
            await Shell.Tell(App.Text.Get("SettingsErrorBar.Title"), problem);
        }
        await ViewModel.LoadAsync();
    }

    private void OnOpenUserLogs(object sender, RoutedEventArgs e) => Shell.OpenFolder(Shell.UserLogFolder);

    private void OnOpenServiceLogs(object sender, RoutedEventArgs e) => Shell.OpenFolder(Shell.ServiceLogFolder(ViewModel.LogDir));
}
