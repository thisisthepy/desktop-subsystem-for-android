namespace Dsa.Setup
{
    /// <summary>The words on the setup screens (English).</summary>
    internal static class Text
    {
        public const string Caption = "Desktop Subsystem for Android\u2122 Setup";
        public const string Title = "Desktop Subsystem for Android\u2122";
        public const string Publisher = "Thisisthepy";
        public const string Architecture = "x64";
        public const string VersionChip = "Version {0}";
        public const string AndroidChip = "Android {0}";

        public const string InstallText =
            "Run Android apps in their own windows on your desktop. Setup installs the DSA service and downloads Android (about 1.8 GB).";
        public const string InstallTextBundledImage =
            "Run Android apps in their own windows on your desktop. Setup installs the DSA service and Android.";
        public const string LocationHeader = "Install location";
        public const string LocationData = "Android images in {0}";
        public const string ChangeButton = "Change";
        public const string LegalBefore = "By installing, you agree to the ";
        public const string LegalLink = "Apache License 2.0";
        public const string LegalAfter =
            ". DSA is an independent open-source project. It is not affiliated with, " +
            "endorsed by or sponsored by Microsoft or Google. Android is a trademark of Google LLC.";
        public const string LicenseUrl = "https://www.apache.org/licenses/LICENSE-2.0";

        public const string OptionsHeader = "Install location";
        public const string OptionsProgramLabel = "Program folder";
        public const string OptionsDataLabel = "Folder for the Android images, settings and logs";
        public const string OptionsDataNote =
            "Each user's Android data is kept in their own profile, under AppData\\Local\\DSA.";
        public const string OptionsInvalid = "Choose a full path, such as C:\\Program Files\\DSA.";
        public const string BrowseButton = "Browse";
        public const string BrowseProgramTitle = "Choose the program folder";
        public const string BrowseDataTitle = "Choose the folder for the Android images, settings and logs";

        public const string ModifyText =
            "Desktop Subsystem for Android is installed. Repair puts back missing or damaged files. Uninstall removes the program and the downloaded Android images, and keeps each user's Android data.";
        public const string InstalledLocation = "Installed in {0}";

        public const string ProgressInstall = "Installing";
        public const string ProgressRepair = "Repairing";
        public const string ProgressUninstall = "Uninstalling";
        public const string ProgressLayout = "Copying the setup files";
        public const string ProgressPreparing = "Preparing\u2026";
        public const string ProgressPreviousVersion = "Removing the previous version\u2026";
        public const string ProgressCancelling = "Cancelling\u2026";

        public const string SuccessInstall = "Installed";
        public const string SuccessInstallText =
            "Android is downloading in the background. Type dsa status in a terminal to follow it.";
        public const string SuccessInstallTextBundledImage =
            "The DSA service is running. Type dsa status in a terminal to see your Android.";
        public const string SuccessRepair = "Repaired";
        public const string SuccessUninstall = "Uninstalled";
        public const string SuccessUninstallText =
            "Each user's Android data is kept in their profile, under AppData\\Local\\DSA, until they delete it.";
        public const string SuccessLayout = "Setup files copied";
        public const string SuccessLayoutText = "The setup files are in {0}.";
        public const string CodeInText = "dsa status";

        public const string RestartText = "Restart the computer to finish.";

        public const string FailureInstall = "Setup did not finish";
        public const string FailureUninstall = "Uninstall did not finish";
        public const string FailureCancelled = "You cancelled setup. It undid what it had done.";
        public const string FailureLogBefore = "The ";
        public const string FailureLogLink = "log file";
        public const string FailureLogAfter = " says what went wrong.";

        public const string NewerHeader = "A newer version is installed";
        public const string NewerText =
            "A newer Desktop Subsystem for Android is already on this computer, so setup has nothing to do.";

        public const string HelpHeader = "Setup help";
        public const string HelpText =
            "/install | /repair | /uninstall | /layout [folder]\n" +
            "    installs, repairs, uninstalls or copies the setup\n" +
            "    files to a folder.\n" +
            "/passive | /quiet\n" +
            "    shows only progress, or nothing at all.\n" +
            "/norestart\n" +
            "    never restarts the computer.\n" +
            "/log log.txt\n" +
            "    writes a log to the file.\n" +
            "InstallFolder=<folder> DataFolder=<folder>\n" +
            "    sets the program folder and the folder for the\n" +
            "    Android images, settings and logs.";

        public const string InstallButton = "Install";
        public const string CancelButton = "Cancel";
        public const string BackButton = "Back";
        public const string CloseButton = "Close";
        public const string DoneButton = "Done";
        public const string OkButton = "OK";
        public const string RepairButton = "Repair";
        public const string UninstallButton = "Uninstall";
        public const string RestartButton = "Restart";
        public const string Minimize = "Minimize";

        public const string ConfirmCancel = "Stop setup?";
    }
}
