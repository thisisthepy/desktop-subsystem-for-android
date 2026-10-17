namespace Dsa.Setup
{
    using System;
    using System.Diagnostics;
    using System.IO;
    using System.Text.RegularExpressions;
    using System.Windows;
    using System.Windows.Interop;
    using System.Windows.Threading;
    using WixToolset.BootstrapperApplicationApi;
    using ErrorEventArgs = WixToolset.BootstrapperApplicationApi.ErrorEventArgs;
    using StartupEventArgs = WixToolset.BootstrapperApplicationApi.StartupEventArgs;

    /// <summary>
    /// The bootstrapper application: it answers the Burn engine and drives the window.
    /// Engine callbacks arrive on the engine's thread; everything the window shows is handed
    /// to the window's own thread through its dispatcher.
    /// </summary>
    internal sealed class SetupApplication : BootstrapperApplication
    {
        /// <summary>The package's id in bundle.wxs.</summary>
        private const string PackageId = "DsaPackage";

        private const int ErrorSuccess = 0;
        private const int ErrorInstallUserExit = 1602;
        private const int ErrorSuccessRebootInitiated = 1641;
        private const int ErrorSuccessRebootRequired = 3010;
        private const int HresultUserExit = unchecked((int)0x80070642);
        private const int HresultCancelled = unchecked((int)0x800704C7);

        private static readonly Regex ActionStart = new Regex(
            @"^Action\s+\d+:\d+:\d+:\s+(?<name>[^.]*)\.\s*(?<text>.*)$",
            RegexOptions.CultureInvariant | RegexOptions.Singleline);

        private readonly object progressLock = new object();

        private IBootstrapperCommand command;
        private IMbaCommand mbaCommand;
        private Dispatcher dispatcher;
        private MainWindow window;
        private HwndSource hiddenWindow;
        private IntPtr parentWindow;

        private volatile bool canceled;
        private volatile bool applying;
        private bool applied;
        private bool installed;
        private bool newerInstalled;
        private bool autoClose;
        private bool restartRequired;
        private bool restartAccepted;
        private int result = ErrorSuccess;
        private string lastError;
        private string layoutDirectory;
        private int phases = 1;
        private int cacheProgress;
        private int executeProgress;

        internal IEngine Engine => this.engine;

        internal Display Display => this.command.Display;

        internal LaunchAction PlannedAction { get; private set; }

        internal string Version { get; private set; }

        /// <summary>The newest Android major the release supports, or null when not known.</summary>
        internal string AndroidMajor { get; private set; }

        /// <summary>Whether the package carries an Android image (else the service downloads one).</summary>
        internal bool BundledImage { get; private set; }

        internal string LayoutDirectory => this.layoutDirectory;

        protected override void OnCreate(CreateEventArgs args)
        {
            base.OnCreate(args);
            this.command = args.Command;
        }

        protected override void OnStartup(StartupEventArgs args)
        {
            // Command line variables (InstallFolder=..., DataFolder=...) are the application's
            // to apply; the engine leaves them alone.
            this.mbaCommand = this.command.ParseCommandLine();
            try
            {
                var dataPath = this.command.BootstrapperApplicationDataPath;
                var data = string.IsNullOrEmpty(dataPath)
                    ? new BootstrapperApplicationData()
                    : new BootstrapperApplicationData(new FileInfo(dataPath));
                this.mbaCommand.SetOverridableVariables(data.Bundle.OverridableVariables, this.engine);
            }
            catch (Exception e)
            {
                this.Log("could not apply the command line variables: " + e.Message);
            }

            this.Version = this.engine.GetVariableVersion("WixBundleVersion");
            this.AndroidMajor = this.ReadVariable("AndroidMajor");
            this.BundledImage = this.ReadVariable("BundledImage") == "1";

            base.OnStartup(args);
        }

        /// <summary>The window's thread: shows the window (unless quiet) and runs until setup ends.</summary>
        protected override void Run()
        {
            this.dispatcher = Dispatcher.CurrentDispatcher;
            this.Log("running the setup screens, display " + this.command.Display + ", action " + this.command.Action +
                     ", Windows build " + Palette.Build + (Palette.HasMica ? ", Mica and rounded corners" : ", solid background"));
            AppDomain.CurrentDomain.UnhandledException += (sender, e) => this.Log("failed: " + e.ExceptionObject);
            this.dispatcher.UnhandledException += (sender, e) =>
            {
                // A fault in the window must not leave the engine waiting forever.
                this.Log("failed: " + e.Exception);
                this.result = unchecked((int)0x8007064C); // ERROR_INSTALL_FAILURE
                e.Handled = true;
                this.dispatcher.InvokeShutdown();
            };

            try
            {
                if (this.command.Display == Display.Full || this.command.Display == Display.Passive)
                {
                    this.window = new MainWindow(this);
                    this.window.Closed += (sender, e) => this.dispatcher.InvokeShutdown();
                    this.window.Show();
                    this.parentWindow = this.window.Handle;
                    this.Log("window manager answered: " + Palette.BackdropResult);
                }
                else
                {
                    // The engine wants a parent window for apply even when nothing is shown.
                    this.hiddenWindow = new HwndSource(new HwndSourceParameters("DsaSetup") { Width = 0, Height = 0, WindowStyle = 0 });
                    this.parentWindow = this.hiddenWindow.Handle;
                }

                this.engine.Detect(this.parentWindow);
                Dispatcher.Run();
            }
            catch (Exception e)
            {
                this.Log("failed: " + e);
                this.result = unchecked((int)0x8007064C);
            }

            int exitCode = this.result;
            if ((exitCode & 0xFFFF0000) == unchecked((int)0x80070000))
            {
                exitCode &= 0xFFFF; // a plain Win32 error, not an HRESULT
            }

            if (exitCode == ErrorSuccess && this.restartRequired)
            {
                exitCode = this.restartAccepted ? ErrorSuccessRebootInitiated : ErrorSuccessRebootRequired;
            }

            this.hiddenWindow?.Dispose();
            this.engine.Quit(exitCode);
        }

        // ---- What the window asks for (on the window's thread) ----

        internal string GetFolder(string variable)
        {
            try
            {
                return this.engine.FormatString(this.engine.GetVariableString(variable));
            }
            catch (Exception)
            {
                return string.Empty;
            }
        }

        internal void SetFolder(string variable, string path)
        {
            this.engine.SetVariableString(variable, path, false);
        }

        internal void Start(LaunchAction action)
        {
            this.PlannedAction = action;
            this.canceled = false;
            this.lastError = null;
            this.window?.ShowProgress(action);
            try
            {
                this.engine.Plan(action);
            }
            catch (Exception e)
            {
                this.Log("could not plan: " + e.Message);
                this.Finish(e.HResult < 0 ? e.HResult : unchecked((int)0x8007064C));
            }
        }

        /// <summary>Cancel during apply. Returns false when the user chose to keep going.</summary>
        internal bool RequestCancel()
        {
            if (!this.applying)
            {
                return true;
            }

            if (this.command.Display == Display.Full && this.window != null &&
                MessageBox.Show(this.window, Text.ConfirmCancel, Text.Caption, MessageBoxButton.YesNo, MessageBoxImage.Question) != MessageBoxResult.Yes)
            {
                return false;
            }

            this.canceled = true;
            this.window?.ShowCancelling();
            return true;
        }

        /// <summary>The window is closing. Returns false to keep it open until apply ends.</summary>
        internal bool CanClose()
        {
            if (this.applying)
            {
                if (this.RequestCancel())
                {
                    this.autoClose = true;
                }

                return false;
            }

            if (!this.applied && this.result == ErrorSuccess)
            {
                this.result = ErrorInstallUserExit;
            }

            return true;
        }

        internal void AcceptRestart()
        {
            this.restartAccepted = true;
        }

        internal void OpenLog()
        {
            Open(this.ReadVariable("WixBundleLog"));
        }

        internal static void Open(string target)
        {
            if (string.IsNullOrEmpty(target))
            {
                return;
            }

            try
            {
                Process.Start(new ProcessStartInfo(target) { UseShellExecute = true })?.Dispose();
            }
            catch (Exception)
            {
                // Nothing to open it with; setup carries on.
            }
        }

        // ---- Detect ----

        protected override void OnDetectBegin(DetectBeginEventArgs args)
        {
            this.installed = args.RegistrationType == RegistrationType.Full;
            base.OnDetectBegin(args);
        }

        protected override void OnDetectRelatedBundle(DetectRelatedBundleEventArgs args)
        {
            if (args.RelationType == RelationType.Upgrade && this.engine.CompareVersions(args.Version, this.Version) > 0)
            {
                this.newerInstalled = true;
            }

            base.OnDetectRelatedBundle(args);
        }

        protected override void OnDetectComplete(DetectCompleteEventArgs args)
        {
            base.OnDetectComplete(args);
            int status = args.Status;
            this.OnWindowThread(() => this.AfterDetect(status));
        }

        private void AfterDetect(int status)
        {
            var display = this.command.Display;
            var action = this.command.Action;
            if (status < 0)
            {
                this.Finish(status);
                return;
            }

            if (action == LaunchAction.Help)
            {
                if (display == Display.Full)
                {
                    this.window.ShowHelp();
                }
                else
                {
                    this.Finish(ErrorSuccess);
                }

                return;
            }

            if (action == LaunchAction.Layout)
            {
                this.StartLayout();
                return;
            }

            if (display != Display.Full)
            {
                this.Start(action == LaunchAction.Unknown ? LaunchAction.Install : action);
                return;
            }

            if (action == LaunchAction.Uninstall && this.installed && this.command.Resume != ResumeType.Arp)
            {
                this.Start(LaunchAction.Uninstall);
            }
            else if (action == LaunchAction.Repair && this.installed)
            {
                this.Start(LaunchAction.Repair);
            }
            else if (this.installed)
            {
                this.window.ShowMaintenance();
            }
            else if (this.newerInstalled)
            {
                this.window.ShowNewer();
            }
            else
            {
                this.window.ShowInstall();
            }
        }

        private void StartLayout()
        {
            var folder = this.command.LayoutDirectory;
            if (string.IsNullOrEmpty(folder))
            {
                folder = this.command.Display == Display.Full
                    ? FolderPicker.Pick(this.window.Handle, Text.ProgressLayout, Directory.GetCurrentDirectory())
                    : Directory.GetCurrentDirectory();
                if (folder == null)
                {
                    this.result = ErrorInstallUserExit;
                    this.window.Close();
                    return;
                }
            }

            this.layoutDirectory = folder;
            this.engine.SetVariableString("WixBundleLayoutDirectory", folder, false);
            this.Start(LaunchAction.Layout);
        }

        // ---- Plan and apply ----

        protected override void OnPlanComplete(PlanCompleteEventArgs args)
        {
            base.OnPlanComplete(args);
            if (args.Status < 0 || this.canceled)
            {
                int status = args.Status < 0 ? args.Status : HresultUserExit;
                this.OnWindowThread(() => this.Finish(status));
                return;
            }

            this.applying = true;
            try
            {
                this.engine.Apply(this.parentWindow);
            }
            catch (Exception e)
            {
                // An engine that refuses to apply calls back no more, so setup ends here.
                this.Log("could not apply: " + e.Message);
                this.applying = false;
                int status = e.HResult < 0 ? e.HResult : unchecked((int)0x8007064C);
                this.OnWindowThread(() => this.Finish(status));
            }
        }

        protected override void OnApplyBegin(ApplyBeginEventArgs args)
        {
            lock (this.progressLock)
            {
                this.phases = Math.Max(1, args.PhaseCount);
                this.cacheProgress = 0;
                this.executeProgress = 0;
            }

            this.OnWindowThread(() => this.window?.SetProgress(0, Text.ProgressPreparing));
            base.OnApplyBegin(args);
        }

        protected override void OnCacheAcquireProgress(CacheAcquireProgressEventArgs args)
        {
            this.CacheProgress(args.OverallPercentage);
            args.Cancel = this.canceled;
            base.OnCacheAcquireProgress(args);
        }

        protected override void OnCacheContainerOrPayloadVerifyProgress(CacheContainerOrPayloadVerifyProgressEventArgs args)
        {
            this.CacheProgress(args.OverallPercentage);
            args.Cancel = this.canceled;
            base.OnCacheContainerOrPayloadVerifyProgress(args);
        }

        protected override void OnCachePayloadExtractProgress(CachePayloadExtractProgressEventArgs args)
        {
            this.CacheProgress(args.OverallPercentage);
            args.Cancel = this.canceled;
            base.OnCachePayloadExtractProgress(args);
        }

        protected override void OnCacheVerifyProgress(CacheVerifyProgressEventArgs args)
        {
            this.CacheProgress(args.OverallPercentage);
            args.Cancel = this.canceled;
            base.OnCacheVerifyProgress(args);
        }

        protected override void OnCacheComplete(CacheCompleteEventArgs args)
        {
            this.CacheProgress(100);
            base.OnCacheComplete(args);
        }

        protected override void OnExecuteProgress(ExecuteProgressEventArgs args)
        {
            int percent;
            lock (this.progressLock)
            {
                this.executeProgress = args.OverallPercentage;
                percent = this.Overall();
            }

            this.ShowPercent(percent, null);
            args.Cancel = this.canceled;
            base.OnExecuteProgress(args);
        }

        protected override void OnProgress(ProgressEventArgs args)
        {
            args.Cancel = this.canceled;
            base.OnProgress(args);
        }

        protected override void OnExecutePackageBegin(ExecutePackageBeginEventArgs args)
        {
            if (args.ShouldExecute && !string.Equals(args.PackageId, PackageId, StringComparison.Ordinal))
            {
                // Anything but our package is the bundle of an older version being removed.
                this.ShowPercent(null, Text.ProgressPreviousVersion);
            }

            args.Cancel = this.canceled;
            base.OnExecutePackageBegin(args);
        }

        protected override void OnExecuteMsiMessage(ExecuteMsiMessageEventArgs args)
        {
            if (args.MessageType == InstallMessage.ActionStart)
            {
                var text = DescribeAction(args);
                if (!string.IsNullOrEmpty(text))
                {
                    this.ShowPercent(null, text + "…");
                }
            }

            if (this.canceled)
            {
                args.Result = Result.Cancel;
            }

            base.OnExecuteMsiMessage(args);
        }

        protected override void OnError(ErrorEventArgs args)
        {
            if (this.canceled)
            {
                args.Result = Result.Cancel;
            }
            else if (args.ErrorType == ErrorType.HttpServerAuthentication || args.ErrorType == ErrorType.HttpProxyAuthentication)
            {
                args.Result = Result.TryAgain;
            }
            else
            {
                this.lastError = args.ErrorMessage;
                int buttons = args.UIHint & 0xF;
                // A plain message is shown on the failure screen; one that asks something
                // (retry, ignore) needs an answer now.
                if (this.command.Display == Display.Full && buttons != 0 && this.window != null)
                {
                    var box = buttons == 1 ? MessageBoxButton.OKCancel
                        : buttons == 3 ? MessageBoxButton.YesNoCancel
                        : buttons == 4 ? MessageBoxButton.YesNo
                        : MessageBoxButton.OK;
                    var answer = MessageBoxResult.None;
                    this.dispatcher.Invoke(() =>
                        answer = MessageBox.Show(this.window, args.ErrorMessage, Text.Caption, box, MessageBoxImage.Error));
                    if ((int)box == buttons)
                    {
                        args.Result = (Result)answer;
                    }
                }
            }

            base.OnError(args);
        }

        protected override void OnApplyComplete(ApplyCompleteEventArgs args)
        {
            base.OnApplyComplete(args);
            this.applying = false;
            this.applied = true;
            this.result = args.Status;
            this.restartRequired = args.Restart == ApplyRestart.RestartRequired;

            var policy = this.mbaCommand.Restart;
            if (policy == Restart.Always || (this.restartRequired && policy == Restart.Automatic))
            {
                this.restartRequired = true;
                this.restartAccepted = true;
                args.Action = BOOTSTRAPPER_APPLYCOMPLETE_ACTION.Restart;
            }

            int status = args.Status;
            bool promptRestart = this.restartRequired && !this.restartAccepted && policy == Restart.Prompt;
            this.OnWindowThread(() => this.AfterApply(status, promptRestart));
        }

        private void AfterApply(int status, bool promptRestart)
        {
            if (this.window == null)
            {
                this.dispatcher.InvokeShutdown();
                return;
            }

            if (this.command.Display != Display.Full || this.autoClose || this.restartAccepted)
            {
                this.window.Close();
                return;
            }

            if (status >= 0)
            {
                this.window.ShowSuccess(this.PlannedAction, promptRestart);
            }
            else
            {
                bool cancelled = status == HresultUserExit || status == HresultCancelled;
                this.window.ShowFailure(this.PlannedAction, cancelled ? null : this.lastError, cancelled, promptRestart);
            }
        }

        protected override void OnShutdown(ShutdownEventArgs args)
        {
            base.OnShutdown(args);
            if (this.restartAccepted && this.restartRequired)
            {
                this.Log("restarting the computer to finish");
                args.Action = BOOTSTRAPPER_SHUTDOWN_ACTION.Restart;
            }
        }

        // ---- Helpers ----

        /// <summary>Ends setup without applying anything (or after a failure before apply).</summary>
        private void Finish(int status)
        {
            this.result = status;
            if (this.window == null)
            {
                this.dispatcher.InvokeShutdown();
            }
            else if (status < 0 && this.command.Display == Display.Full)
            {
                bool cancelled = status == HresultUserExit || status == HresultCancelled;
                this.window.ShowFailure(this.PlannedAction, cancelled ? null : this.lastError, cancelled, false);
            }
            else
            {
                this.window.Close();
            }
        }

        private void CacheProgress(int overall)
        {
            int percent;
            lock (this.progressLock)
            {
                this.cacheProgress = overall;
                percent = this.Overall();
            }

            this.ShowPercent(percent, null);
        }

        private int Overall() => Math.Min(100, (this.cacheProgress + this.executeProgress) / this.phases);

        private void ShowPercent(int? percent, string detail)
        {
            if (this.window != null)
            {
                this.OnWindowThread(() => this.window.SetProgress(percent, detail));
            }
        }

        /// <summary>The words Windows Installer gives the action it is starting.</summary>
        private static string DescribeAction(ExecuteMsiMessageEventArgs args)
        {
            var match = ActionStart.Match(args.Message ?? string.Empty);
            if (match.Success)
            {
                return match.Groups["text"].Value.Trim();
            }

            return args.Data != null && args.Data.Count > 1 ? args.Data[1]?.Trim() : null;
        }

        private void OnWindowThread(Action action)
        {
            this.dispatcher?.BeginInvoke(action);
        }

        private string ReadVariable(string name)
        {
            try
            {
                return this.engine.ContainsVariable(name) ? this.engine.GetVariableString(name) : null;
            }
            catch (Exception)
            {
                return null;
            }
        }

        private void Log(string message)
        {
            this.engine.Log(LogLevel.Standard, "DsaSetup: " + message);
        }
    }
}
