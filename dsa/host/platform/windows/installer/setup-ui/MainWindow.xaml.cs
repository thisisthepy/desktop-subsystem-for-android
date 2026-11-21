namespace Dsa.Setup
{
    using System;
    using System.ComponentModel;
    using System.IO;
    using System.Linq;
    using System.Windows;
    using System.Windows.Automation;
    using System.Windows.Automation.Peers;
    using System.Windows.Controls;
    using System.Windows.Documents;
    using System.Windows.Interop;
    using System.Windows.Media;
    using System.Windows.Media.Imaging;
    using Shape = System.Windows.Shapes.Shape;
    using WixToolset.BootstrapperApplicationApi;

    /// <summary>
    /// The one setup window. Every screen shares the caption, the hero and the footer; a
    /// screen is the panel under the hero and the buttons in the footer.
    /// </summary>
    public partial class MainWindow : Window
    {
        private const int WmSettingChange = 0x001A;
        private const string InstallFolder = "InstallFolder";
        private const string DataFolder = "DataFolder";

        private readonly SetupApplication app;
        private bool dark;
        private Screen screen;

        internal MainWindow(SetupApplication app)
        {
            this.app = app;
            this.InitializeComponent();
            // The window's language picks the font WPF falls back to for the characters Segoe UI
            // lacks: Yu Gothic for Japanese, Microsoft YaHei or JhengHei for Chinese, Malgun
            // Gothic for Korean, so each draws in its own forms.
            this.Language = System.Windows.Markup.XmlLanguage.GetLanguage(Text.Language.Tag);
            this.dark = Palette.IsDark();
            Palette.Apply(this, this.dark);
            if (!Palette.HasMica)
            {
                // Without Mica a full glass frame would show black; keep a 1 px edge so the
                // window manager still draws the shadow.
                this.Chrome.GlassFrameThickness = new Thickness(0, 0, 0, 1);
            }

            this.Icon = BitmapFrame.Create(new Uri("pack://application:,,,/assets/logo.ico"));
            this.CaptionIcon.Source = IconFrame(16);
            this.Fill();
            this.Show(Screen.Detecting);
        }

        private enum Screen
        {
            Detecting,
            Install,
            Options,
            Maintenance,
            Progress,
            Result,
            Help,
        }

        internal IntPtr Handle => new WindowInteropHelper(this).Handle;

        // ---- Screens ----

        internal void ShowInstall()
        {
            this.FillLocation();
            this.Show(Screen.Install);
        }

        internal void ShowMaintenance()
        {
            this.MaintenanceLocation.Text = string.Format(Text.InstalledLocation, this.app.GetFolder(InstallFolder));
            this.Show(Screen.Maintenance);
        }

        internal void ShowNewer()
        {
            this.SetResult(Badge.Info, Text.NewerHeader, Text.NewerText, false);
            this.Show(Screen.Result, close: true);
            this.Announce(ScreenReader.Announcement(Text.NewerHeader, Text.NewerText));
        }

        internal void ShowHelp()
        {
            this.Show(Screen.Help, close: true);
        }

        internal void ShowProgress(LaunchAction action)
        {
            this.ProgressTitle.Text =
                action == LaunchAction.Uninstall ? Text.ProgressUninstall
                : action == LaunchAction.Repair ? Text.ProgressRepair
                : action == LaunchAction.Layout ? Text.ProgressLayout
                : Text.ProgressInstall;
            // The bar is named for what it measures, so a screen reader says "Installing, 40".
            AutomationProperties.SetName(this.Progress, this.ProgressTitle.Text);
            this.Progress.Value = 0;
            this.ProgressPercent.Text = "0%";
            this.ProgressDetail.Text = Text.ProgressPreparing;
            this.CancelButton.IsEnabled = true;
            this.Show(Screen.Progress);
        }

        internal void SetProgress(int? percent, string detail)
        {
            if (percent.HasValue)
            {
                this.Progress.Value = percent.Value;
                this.ProgressPercent.Text = percent.Value + "%";
            }

            if (detail != null && this.CancelButton.IsEnabled)
            {
                this.ProgressDetail.Text = detail;
            }
        }

        internal void ShowCancelling()
        {
            this.ProgressDetail.Text = Text.ProgressCancelling;
            this.CancelButton.IsEnabled = false;
        }

        internal void ShowSuccess(LaunchAction action, bool restart)
        {
            string title;
            string text;
            switch (action)
            {
                case LaunchAction.Uninstall:
                    title = Text.SuccessUninstall;
                    text = Text.SuccessUninstallText;
                    break;
                case LaunchAction.Repair:
                    title = Text.SuccessRepair;
                    text = Text.SuccessInstallTextBundledImage;
                    break;
                case LaunchAction.Layout:
                    title = Text.SuccessLayout;
                    text = string.Format(Text.SuccessLayoutText, this.app.LayoutDirectory);
                    break;
                default:
                    title = Text.SuccessInstall;
                    text = this.app.BundledImage ? Text.SuccessInstallTextBundledImage : Text.SuccessInstallText;
                    break;
            }

            this.SetResult(Badge.Success, title, text, restart);
            this.Show(Screen.Result, done: !restart, close: restart, restart: restart);
            this.Announce(ScreenReader.Announcement(title, text, restart ? Text.RestartText : null));
        }

        internal void ShowFailure(LaunchAction action, string message, bool cancelled, bool restart)
        {
            var title = action == LaunchAction.Uninstall ? Text.FailureUninstall : Text.FailureInstall;
            this.SetResult(Badge.Error, title, null, restart);
            var text = this.ResultText.Inlines;
            string announcement;
            if (cancelled)
            {
                text.Add(new Run(Text.FailureCancelled));
                announcement = ScreenReader.Announcement(title, Text.FailureCancelled, restart ? Text.RestartText : null);
            }
            else
            {
                if (!string.IsNullOrWhiteSpace(message))
                {
                    text.Add(new Run(message.Trim()));
                    text.Add(new Run(" "));
                }

                text.Add(new Run(Text.FailureLogBefore));
                text.Add(this.Link(Text.FailureLogLink, this.app.OpenLog));
                text.Add(new Run(Text.FailureLogAfter));
                announcement = ScreenReader.Announcement(title, message,
                    Text.FailureLogBefore + Text.FailureLogLink + Text.FailureLogAfter, restart ? Text.RestartText : null);
            }

            this.Show(Screen.Result, close: true, restart: restart);
            this.Announce(announcement);
        }

        /// <summary>
        /// Tells a screen reader how setup went, once the result screen is laid out. A UI
        /// Automation notification where the .NET Framework has one (4.8 and later); on 4.7.2 the
        /// result title is a live region, which is told that it changed.
        /// </summary>
        private void Announce(string words)
        {
            this.Dispatcher.BeginInvoke(System.Windows.Threading.DispatcherPriority.ContextIdle, new Action(() =>
            {
                var peer = UIElementAutomationPeer.FromElement(this.ResultTitle)
                    ?? UIElementAutomationPeer.CreatePeerForElement(this.ResultTitle);
                if (peer == null || RaiseNotification(peer, words))
                {
                    return;
                }

                AutomationProperties.SetName(this.ResultTitle, words);
                peer.RaiseAutomationEvent(AutomationEvents.LiveRegionChanged);
            }));
        }

        /// <summary>
        /// Raises a UI Automation notification with the words. The method came in .NET Framework
        /// 4.8, after the 4.7.2 this window is built for, so it is looked up on the framework it
        /// runs on. False when that framework has none.
        /// </summary>
        private static bool RaiseNotification(AutomationPeer peer, string words)
        {
            var method = typeof(AutomationPeer).GetMethod("RaiseNotificationEvent");
            var parameters = method?.GetParameters();
            if (parameters == null || parameters.Length != 4 || !parameters[0].ParameterType.IsEnum || !parameters[1].ParameterType.IsEnum)
            {
                return false;
            }

            try
            {
                var kind = Enum.Parse(parameters[0].ParameterType, "ActionCompleted");
                var processing = Enum.Parse(parameters[1].ParameterType, "ImportantMostRecent");
                method.Invoke(peer, new object[] { kind, processing, words, "DsaSetupResult" });
                return true;
            }
            catch (Exception e) when (e is ArgumentException || e is System.Reflection.TargetInvocationException)
            {
                return false;
            }
        }

        /// <summary>
        /// Moves the keyboard focus to the new screen's first control, so that a keyboard or
        /// screen reader user is not left on a button the screen has just hidden.
        /// </summary>
        private void FocusFirstControl(Screen screen)
        {
            var fields = screen == Screen.Options ? new UIElement[] { this.ProgramFolderBox } : new UIElement[0];
            var first = ScreenReader.FirstControl<UIElement>(
                fields,
                this.Footer.Children.OfType<Button>(),
                c => c.Visibility == Visibility.Visible && c.IsEnabled,
                c => c is Button button && button.IsDefault);
            if (first != null)
            {
                // The focus scope keeps it too, so the window gives it back when it is activated.
                System.Windows.Input.FocusManager.SetFocusedElement(this, first);
                first.Focus();
            }
        }

        private void Show(Screen screen, bool done = false, bool close = false, bool restart = false)
        {
            this.Hero.Visibility = screen == Screen.Options || screen == Screen.Help ? Visibility.Collapsed : Visibility.Visible;
            this.InstallPanel.Visibility = Visible(screen == Screen.Install || screen == Screen.Detecting);
            this.OptionsPanel.Visibility = Visible(screen == Screen.Options);
            this.MaintenancePanel.Visibility = Visible(screen == Screen.Maintenance);
            this.ProgressPanel.Visibility = Visible(screen == Screen.Progress);
            this.ResultPanel.Visibility = Visible(screen == Screen.Result);
            this.HelpPanel.Visibility = Visible(screen == Screen.Help);

            bool passive = this.app.Display != Display.Full;
            this.CancelButton.Visibility = Visible(screen == Screen.Detecting || screen == Screen.Install || screen == Screen.Options || screen == Screen.Progress);
            // The folders page is a step before installation, so its way out goes back.
            this.CancelButton.Content = screen == Screen.Options ? Text.BackButton : Text.CancelButton;
            this.CloseButton.Visibility = Visible(screen == Screen.Maintenance || close);
            this.UninstallButton.Visibility = Visible(screen == Screen.Maintenance);
            this.RepairButton.Visibility = Visible(screen == Screen.Maintenance);
            this.OkButton.Visibility = Visible(screen == Screen.Options);
            this.RestartButton.Visibility = Visible(restart);
            this.InstallButton.Visibility = Visible(screen == Screen.Detecting || screen == Screen.Install);
            this.InstallButton.IsEnabled = screen == Screen.Install;
            this.ChangeButton.IsEnabled = screen == Screen.Install;
            this.DoneButton.Visibility = Visible(done);

            // Enter presses the screen's main button; Escape its way out.
            this.InstallButton.IsDefault = screen == Screen.Install;
            this.OkButton.IsDefault = screen == Screen.Options;
            this.RepairButton.IsDefault = screen == Screen.Maintenance;
            this.DoneButton.IsDefault = done;
            this.RestartButton.IsDefault = restart;
            this.CancelButton.IsCancel = this.CancelButton.Visibility == Visibility.Visible;
            this.CloseButton.IsCancel = !this.CancelButton.IsCancel;

            if (passive)
            {
                // Passive setup shows progress only: nothing to choose, only Cancel.
                foreach (var button in this.Footer.Children.OfType<Button>().Where(b => b != this.CancelButton))
                {
                    button.Visibility = Visibility.Collapsed;
                }
            }

            this.screen = screen;
            this.FocusFirstControl(screen);
        }

        private enum Badge
        {
            Success,
            Error,
            Info,
        }

        private void SetResult(Badge badge, string title, string text, bool restart)
        {
            this.ResultBadge.SetResourceReference(Shape.FillProperty,
                badge == Badge.Success ? "Accent" : badge == Badge.Error ? "Critical" : "Neutral");
            this.ResultGlyph.SetResourceReference(Shape.StrokeProperty,
                badge == Badge.Success ? "AccentText" : badge == Badge.Error ? "CriticalText" : "NeutralText");
            this.ResultGlyph.Data = Geometry.Parse(
                badge == Badge.Success ? "M3.5,8.5 L6.5,11.5 L12.5,4.5"
                : badge == Badge.Error ? "M5.5,5.5 L10.5,10.5 M10.5,5.5 L5.5,10.5"
                : "M8,7.5 L8,11.5 M8,4.6 L8,4.7");
            this.ResultTitle.Text = title;
            this.ResultText.Inlines.Clear();
            if (text != null)
            {
                this.AddWithCode(this.ResultText.Inlines, text);
            }

            this.ResultRestart.Visibility = Visible(restart);
        }

        // ---- Filling in ----

        private void Fill()
        {
            this.Title = Text.Caption;
            this.CaptionTitle.Text = Text.Caption;
            this.HeroTitle.Text = Text.Title;
            this.HeroPublisher.Text = Text.Publisher;
            this.VersionChip.Text = string.Format(Text.VersionChip, this.app.Version);
            this.ArchitectureChip.Text = Text.Architecture;
            if (string.IsNullOrEmpty(this.app.AndroidMajor))
            {
                this.AndroidChipFrame.Visibility = Visibility.Collapsed;
            }
            else
            {
                this.AndroidChip.Text = string.Format(
                    this.app.AndroidPinned ? Text.AndroidChipPinned : Text.AndroidChip, this.app.AndroidMajor);
            }

            this.FillLocation();
            this.InstallText.Text = this.app.BundledImage ? Text.InstallTextBundledImage : Text.InstallText;
            this.LegalText.Inlines.Add(new Run(Text.LegalBefore));
            this.LegalText.Inlines.Add(this.Link(Text.LegalLink, () => SetupApplication.Open(Text.LicenseUrl)));
            this.LegalText.Inlines.Add(new Run(Text.LegalAfter));
            this.OptionsNote.Text = Text.OptionsDataNote;
            this.MaintenanceText.Text = Text.ModifyText;
            this.HelpText.Text = Text.HelpText;
            this.ResultRestart.Text = Text.RestartText;
            this.LocationHeader.Text = Text.LocationHeader;
            this.OptionsHeader.Text = Text.OptionsHeader;
            this.OptionsProgramLabel.Text = Text.OptionsProgramLabel;
            this.OptionsDataLabel.Text = Text.OptionsDataLabel;
            AutomationProperties.SetName(this.ProgramFolderBox, Text.OptionsProgramLabel);
            AutomationProperties.SetName(this.DataFolderBox, Text.OptionsDataLabel);
            this.HelpHeader.Text = Text.HelpHeader;
            this.ChangeButton.Content = Text.ChangeButton;
            this.BrowseProgram.Content = Text.BrowseButton;
            this.BrowseData.Content = Text.BrowseButton;
            this.CancelButton.Content = Text.CancelButton;
            this.CloseButton.Content = Text.CloseButton;
            this.UninstallButton.Content = Text.UninstallButton;
            this.RepairButton.Content = Text.RepairButton;
            this.OkButton.Content = Text.OkButton;
            this.RestartButton.Content = Text.RestartButton;
            this.InstallButton.Content = Text.InstallButton;
            this.DoneButton.Content = Text.DoneButton;
            AutomationProperties.SetName(this.CaptionMinimize, Text.Minimize);
            AutomationProperties.SetName(this.CaptionClose, Text.CloseButton);
            // The logo is an image to a screen reader; it says whose it is.
            AutomationProperties.SetName(this.CaptionIcon, Text.Logo);
            AutomationProperties.SetName(this.HeroLogo, Text.Logo);
            // The two Browse buttons and Change keep the words they show, which voice control
            // users say; their help says which folder each one picks.
            AutomationProperties.SetHelpText(this.ChangeButton, Text.LocationHeader);
            AutomationProperties.SetHelpText(this.BrowseProgram, Text.BrowseProgramTitle);
            AutomationProperties.SetHelpText(this.BrowseData, Text.BrowseDataTitle);
            // The progress bar's name until a task names it for what it measures.
            AutomationProperties.SetName(this.Progress, Text.ProgressInstall);
        }

        private void FillLocation()
        {
            this.LocationProgram.Text = this.app.GetFolder(InstallFolder);
            this.LocationData.Text = string.Format(Text.LocationData, this.app.GetFolder(DataFolder));
        }

        private Hyperlink Link(string text, Action open)
        {
            var link = new Hyperlink(new Run(text)) { Style = (Style)this.FindResource("LinkStyle") };
            link.Click += (sender, e) => open();
            return link;
        }

        /// <summary>Adds the text, showing "dsa status" as code in a chip.</summary>
        private void AddWithCode(InlineCollection inlines, string text)
        {
            int at = text.IndexOf(Text.CodeInText, StringComparison.Ordinal);
            if (at < 0)
            {
                inlines.Add(new Run(text));
                return;
            }

            inlines.Add(new Run(text.Substring(0, at)));
            var code = new TextBlock
            {
                Text = Text.CodeInText,
                FontFamily = (FontFamily)this.FindResource("CodeFont"),
                FontSize = 12,
                LineHeight = 16,
                LineStackingStrategy = LineStackingStrategy.BlockLineHeight,
            };
            code.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            var chip = new Border { CornerRadius = new CornerRadius(4), Padding = new Thickness(4, 1, 4, 1), Child = code };
            chip.SetResourceReference(Border.BackgroundProperty, "ChipFill");
            inlines.Add(new InlineUIContainer(chip) { BaselineAlignment = BaselineAlignment.Center });
            inlines.Add(new Run(text.Substring(at + Text.CodeInText.Length)));
        }

        private ImageSource IconFrame(int size)
        {
            var decoder = BitmapDecoder.Create(new Uri("pack://application:,,,/assets/logo.ico"),
                BitmapCreateOptions.None, BitmapCacheOption.OnLoad);
            double scale = VisualTreeHelper.GetDpi(this).DpiScaleX;
            int wanted = (int)Math.Ceiling(size * scale);
            return decoder.Frames.Where(f => f.PixelWidth >= wanted).OrderBy(f => f.PixelWidth).FirstOrDefault()
                ?? decoder.Frames.OrderByDescending(f => f.PixelWidth).First();
        }

        private static Visibility Visible(bool visible) => visible ? Visibility.Visible : Visibility.Collapsed;

        // ---- Window events ----

        protected override void OnSourceInitialized(EventArgs e)
        {
            base.OnSourceInitialized(e);
            var source = (HwndSource)PresentationSource.FromVisual(this);
            if (Palette.HasMica)
            {
                // The Mica material shows only through a transparent surface.
                source.CompositionTarget.BackgroundColor = Colors.Transparent;
                HideNativeCaptionButtons(source.Handle);
            }

            Palette.ApplyBackdrop(source.Handle, this.dark);
            source.AddHook(this.WndProc);
        }

        /// <summary>
        /// The window manager draws its own caption buttons on a frame that covers the whole
        /// window, and they show through the transparent Mica surface beside this window's
        /// own. Without the system menu style it draws none.
        /// </summary>
        private static void HideNativeCaptionButtons(IntPtr handle)
        {
            const int GwlStyle = -16;
            const int WsSysMenu = 0x00080000;
            int style = GetWindowLong(handle, GwlStyle);
            if ((style & WsSysMenu) != 0)
            {
                SetWindowLong(handle, GwlStyle, style & ~WsSysMenu);
            }
        }

        [System.Runtime.InteropServices.DllImport("user32.dll", EntryPoint = "GetWindowLongW")]
        private static extern int GetWindowLong(IntPtr hwnd, int index);

        [System.Runtime.InteropServices.DllImport("user32.dll", EntryPoint = "SetWindowLongW")]
        private static extern int SetWindowLong(IntPtr hwnd, int index, int value);

        protected override void OnStateChanged(EventArgs e)
        {
            base.OnStateChanged(e);
            if (Palette.HasMica)
            {
                HideNativeCaptionButtons(this.Handle);
            }
        }

        protected override void OnDpiChanged(DpiScale oldDpi, DpiScale newDpi)
        {
            base.OnDpiChanged(oldDpi, newDpi);
            this.CaptionIcon.Source = IconFrame(16);
        }

        private IntPtr WndProc(IntPtr hwnd, int msg, IntPtr wParam, IntPtr lParam, ref bool handled)
        {
            // Windows says "ImmersiveColorSet" when the app theme changes.
            if (msg == WmSettingChange && lParam != IntPtr.Zero &&
                System.Runtime.InteropServices.Marshal.PtrToStringUni(lParam) == "ImmersiveColorSet")
            {
                bool nowDark = Palette.IsDark();
                if (nowDark != this.dark)
                {
                    this.dark = nowDark;
                    Palette.Apply(this, this.dark);
                }
            }

            return IntPtr.Zero;
        }

        protected override void OnActivated(EventArgs e)
        {
            base.OnActivated(e);
            this.CaptionBar.Opacity = 1;
        }

        protected override void OnDeactivated(EventArgs e)
        {
            base.OnDeactivated(e);
            // An inactive window's caption is dimmed, as Windows does.
            this.CaptionBar.Opacity = 0.6;
        }

        protected override void OnClosing(CancelEventArgs e)
        {
            base.OnClosing(e);
            if (!this.app.CanClose())
            {
                e.Cancel = true;
            }
        }

        private void OnCaptionMinimize(object sender, RoutedEventArgs e)
        {
            this.WindowState = WindowState.Minimized;
        }

        private void OnCaptionClose(object sender, RoutedEventArgs e)
        {
            this.Close();
        }

        private void OnInstall(object sender, RoutedEventArgs e)
        {
            this.app.Start(LaunchAction.Install);
        }

        private void OnRepair(object sender, RoutedEventArgs e)
        {
            this.app.Start(LaunchAction.Repair);
        }

        private void OnUninstall(object sender, RoutedEventArgs e)
        {
            this.app.Start(LaunchAction.Uninstall);
        }

        private void OnCancel(object sender, RoutedEventArgs e)
        {
            switch (this.screen)
            {
                case Screen.Options:
                    this.ShowInstall();
                    break;
                case Screen.Progress:
                    this.app.RequestCancel();
                    break;
                default:
                    this.Close();
                    break;
            }
        }

        private void OnClose(object sender, RoutedEventArgs e)
        {
            this.Close();
        }

        private void OnRestart(object sender, RoutedEventArgs e)
        {
            this.app.AcceptRestart();
            this.Close();
        }

        private void OnChange(object sender, RoutedEventArgs e)
        {
            this.ProgramFolderBox.Text = this.app.GetFolder(InstallFolder);
            this.DataFolderBox.Text = this.app.GetFolder(DataFolder);
            // Showing the screen puts the focus in its first field.
            this.Show(Screen.Options);
            this.ProgramFolderBox.SelectAll();
        }

        private void OnBrowseProgram(object sender, RoutedEventArgs e)
        {
            this.Browse(this.ProgramFolderBox, Text.BrowseProgramTitle);
        }

        private void OnBrowseData(object sender, RoutedEventArgs e)
        {
            this.Browse(this.DataFolderBox, Text.BrowseDataTitle);
        }

        private void Browse(TextBox box, string title)
        {
            try
            {
                var folder = FolderPicker.Pick(this.Handle, title, box.Text);
                if (folder != null)
                {
                    box.Text = folder;
                }
            }
            catch (Exception ex)
            {
                MessageBox.Show(this, ex.Message, Text.Caption, MessageBoxButton.OK, MessageBoxImage.Error);
            }
        }

        private void OnOptionsOk(object sender, RoutedEventArgs e)
        {
            var program = Clean(this.ProgramFolderBox.Text);
            var data = Clean(this.DataFolderBox.Text);
            if (program == null || data == null)
            {
                MessageBox.Show(this, Text.OptionsInvalid, Text.Caption, MessageBoxButton.OK, MessageBoxImage.Warning);
                (program == null ? this.ProgramFolderBox : this.DataFolderBox).Focus();
                return;
            }

            this.app.SetFolder(InstallFolder, program);
            this.app.SetFolder(DataFolder, data);
            this.ShowInstall();
        }

        /// <summary>A full local path without a trailing separator, or null.</summary>
        private static string Clean(string path)
        {
            try
            {
                path = (path ?? string.Empty).Trim().Trim('"');
                if (path.Length < 3 || !Path.IsPathRooted(path) || path.StartsWith(@"\\", StringComparison.Ordinal))
                {
                    return null;
                }

                path = Path.GetFullPath(path);
                return path.Length > 3 ? path.TrimEnd('\\') : path;
            }
            catch (Exception)
            {
                return null;
            }
        }
    }
}
