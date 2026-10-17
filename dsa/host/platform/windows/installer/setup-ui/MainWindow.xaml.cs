namespace Dsa.Setup
{
    using System;
    using System.ComponentModel;
    using System.IO;
    using System.Linq;
    using System.Windows;
    using System.Windows.Automation;
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
            this.InstallButton.Focus();
        }

        internal void ShowMaintenance()
        {
            this.MaintenanceLocation.Text = string.Format(Text.InstalledLocation, this.app.GetFolder(InstallFolder));
            this.Show(Screen.Maintenance);
            this.RepairButton.Focus();
        }

        internal void ShowNewer()
        {
            this.SetResult(Badge.Info, Text.NewerHeader, Text.NewerText, false);
            this.Show(Screen.Result, close: true);
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
            switch (action)
            {
                case LaunchAction.Uninstall:
                    this.SetResult(Badge.Success, Text.SuccessUninstall, Text.SuccessUninstallText, restart);
                    break;
                case LaunchAction.Repair:
                    this.SetResult(Badge.Success, Text.SuccessRepair, Text.SuccessInstallTextBundledImage, restart);
                    break;
                case LaunchAction.Layout:
                    this.SetResult(Badge.Success, Text.SuccessLayout, string.Format(Text.SuccessLayoutText, this.app.LayoutDirectory), restart);
                    break;
                default:
                    this.SetResult(Badge.Success, Text.SuccessInstall,
                        this.app.BundledImage ? Text.SuccessInstallTextBundledImage : Text.SuccessInstallText, restart);
                    break;
            }

            this.Show(Screen.Result, done: !restart, close: restart, restart: restart);
        }

        internal void ShowFailure(LaunchAction action, string message, bool cancelled, bool restart)
        {
            var title = action == LaunchAction.Uninstall ? Text.FailureUninstall : Text.FailureInstall;
            this.SetResult(Badge.Error, title, null, restart);
            var text = this.ResultText.Inlines;
            if (cancelled)
            {
                text.Add(new Run(Text.FailureCancelled));
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
            }

            this.Show(Screen.Result, close: true, restart: restart);
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
                this.AndroidChip.Text = string.Format(Text.AndroidChip, this.app.AndroidMajor);
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
            }

            Palette.ApplyBackdrop(source.Handle, this.dark);
            source.AddHook(this.WndProc);
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
            this.Show(Screen.Options);
            this.ProgramFolderBox.Focus();
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
