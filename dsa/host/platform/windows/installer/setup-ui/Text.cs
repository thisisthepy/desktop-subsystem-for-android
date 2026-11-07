using System.Globalization;

namespace Dsa.Setup
{
    /// <summary>The words on the setup screens, in the user's display language: Korean on a
    /// Korean desktop, English everywhere else. Android, DSA and the product name are never
    /// translated.</summary>
    internal static class Text
    {
        /// <summary>Whether the screens are in Korean, from the user's display language.</summary>
        public static readonly bool Korean =
            CultureInfo.CurrentUICulture.TwoLetterISOLanguageName == "ko";

        private static string Pick(string english, string korean) => Korean ? korean : english;

        public static string Caption => Pick("Desktop Subsystem for Android\u2122 Setup", "Desktop Subsystem for Android\u2122 설치");
        public const string Title = "Desktop Subsystem for Android\u2122";
        public const string Publisher = "ThisIsThePy";
        public const string Architecture = "x64";
        public static string VersionChip => Pick("Version {0}", "버전 {0}");
        public const string AndroidChip = "Android {0}";

        public static string InstallText => Pick(
            "Run Android apps in their own windows on your desktop. Setup installs the DSA service and downloads Android (about 1.8 GB).",
            "Android 앱을 데스크톱에서 각자의 창으로 실행합니다. 설치 프로그램이 DSA 서비스를 설치하고 Android를 내려받습니다(약 1.8GB).");
        public static string InstallTextBundledImage => Pick(
            "Run Android apps in their own windows on your desktop. Setup installs the DSA service and Android.",
            "Android 앱을 데스크톱에서 각자의 창으로 실행합니다. 설치 프로그램이 DSA 서비스와 Android를 설치합니다.");
        public static string LocationHeader => Pick("Install location", "설치 위치");
        public static string LocationData => Pick("Android images in {0}", "Android 이미지 위치: {0}");
        public static string ChangeButton => Pick("Change", "변경");
        public static string LegalBefore => Pick("By installing, you agree to the ", "설치하면 ");
        public const string LegalLink = "Apache License 2.0";
        public static string LegalAfter => Pick(
            ". DSA is an independent open-source project. It is not affiliated with, " +
            "endorsed by or sponsored by Microsoft or Google. Android is a trademark of Google LLC.",
            "에 동의하게 됩니다. DSA는 독립 오픈 소스 프로젝트이며 Microsoft나 Google과 " +
            "제휴하거나 그 보증이나 후원을 받지 않습니다. Android는 Google LLC의 상표입니다.");
        public const string LicenseUrl = "https://www.apache.org/licenses/LICENSE-2.0";

        public static string OptionsHeader => Pick("Install location", "설치 위치");
        public static string OptionsProgramLabel => Pick("Program folder", "프로그램 폴더");
        public static string OptionsDataLabel => Pick(
            "Folder for the Android images, settings and logs",
            "Android 이미지, 설정, 로그를 둘 폴더");
        public static string OptionsDataNote => Pick(
            "Each user's Android data is kept in their own profile, under AppData\\Local\\DSA.",
            "사용자마다 Android 데이터는 각자의 프로필 아래 AppData\\Local\\DSA에 보관됩니다.");
        public static string OptionsInvalid => Pick(
            "Choose a full path, such as C:\\Program Files\\DSA.",
            "C:\\Program Files\\DSA처럼 전체 경로를 고르세요.");
        public static string BrowseButton => Pick("Browse", "찾아보기");
        public static string BrowseProgramTitle => Pick("Choose the program folder", "프로그램 폴더 선택");
        public static string BrowseDataTitle => Pick(
            "Choose the folder for the Android images, settings and logs",
            "Android 이미지, 설정, 로그를 둘 폴더 선택");

        public static string ModifyText => Pick(
            "Desktop Subsystem for Android is installed. Repair puts back missing or damaged files. Uninstall removes the program and the downloaded Android images, and keeps each user's Android data.",
            "Desktop Subsystem for Android가 설치되어 있습니다. 복구하면 없어지거나 손상된 파일을 되돌립니다. 제거하면 프로그램과 내려받은 Android 이미지를 지우고, 사용자마다의 Android 데이터는 남깁니다.");
        public static string InstalledLocation => Pick("Installed in {0}", "설치 위치: {0}");

        public static string ProgressInstall => Pick("Installing", "설치하는 중");
        public static string ProgressRepair => Pick("Repairing", "복구하는 중");
        public static string ProgressUninstall => Pick("Uninstalling", "제거하는 중");
        public static string ProgressLayout => Pick("Copying the setup files", "설치 파일을 복사하는 중");
        public static string ProgressPreparing => Pick("Preparing\u2026", "준비하는 중\u2026");
        public static string ProgressPreviousVersion => Pick("Removing the previous version\u2026", "이전 버전을 제거하는 중\u2026");
        public static string ProgressCancelling => Pick("Cancelling\u2026", "취소하는 중\u2026");

        public static string SuccessInstall => Pick("Installed", "설치됨");
        public static string SuccessInstallText => Pick(
            "Android is downloading in the background. Type dsa status in a terminal to follow it.",
            "Android를 백그라운드에서 내려받고 있습니다. 진행 상황은 터미널에서 dsa status를 입력해 확인하세요.");
        public static string SuccessInstallTextBundledImage => Pick(
            "The DSA service is running. Type dsa status in a terminal to see your Android.",
            "DSA 서비스가 실행 중입니다. 내 Android 상태는 터미널에서 dsa status를 입력해 확인하세요.");
        public static string SuccessRepair => Pick("Repaired", "복구됨");
        public static string SuccessUninstall => Pick("Uninstalled", "제거됨");
        public static string SuccessUninstallText => Pick(
            "Each user's Android data is kept in their profile, under AppData\\Local\\DSA, until they delete it.",
            "사용자마다 Android 데이터는 직접 지울 때까지 각자의 프로필 아래 AppData\\Local\\DSA에 남습니다.");
        public static string SuccessLayout => Pick("Setup files copied", "설치 파일 복사됨");
        public static string SuccessLayoutText => Pick("The setup files are in {0}.", "설치 파일 위치: {0}");
        public const string CodeInText = "dsa status";

        public static string RestartText => Pick("Restart the computer to finish.", "마치려면 컴퓨터를 다시 시작하세요.");

        public static string FailureInstall => Pick("Setup did not finish", "설치를 마치지 못했습니다");
        public static string FailureUninstall => Pick("Uninstall did not finish", "제거를 마치지 못했습니다");
        public static string FailureCancelled => Pick(
            "You cancelled setup. It undid what it had done.",
            "설치를 취소했습니다. 그때까지 한 일은 되돌렸습니다.");
        public static string FailureLogBefore => Pick("The ", "");
        public static string FailureLogLink => Pick("log file", "로그 파일");
        public static string FailureLogAfter => Pick(" says what went wrong.", "에서 무엇이 잘못됐는지 볼 수 있습니다.");

        public static string NewerHeader => Pick("A newer version is installed", "더 새 버전이 설치되어 있습니다");
        public static string NewerText => Pick(
            "A newer Desktop Subsystem for Android is already on this computer, so setup has nothing to do.",
            "이 컴퓨터에 더 새 Desktop Subsystem for Android가 이미 있어 설치할 것이 없습니다.");

        public static string HelpHeader => Pick("Setup help", "설치 도움말");
        public static string HelpText => Pick(
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
            "    Android images, settings and logs.",
            "/install | /repair | /uninstall | /layout [폴더]\n" +
            "    설치, 복구, 제거하거나 설치 파일을 폴더에\n" +
            "    복사합니다.\n" +
            "/passive | /quiet\n" +
            "    진행 상황만 보이거나 아무것도 보이지 않습니다.\n" +
            "/norestart\n" +
            "    컴퓨터를 다시 시작하지 않습니다.\n" +
            "/log log.txt\n" +
            "    로그를 파일에 씁니다.\n" +
            "InstallFolder=<폴더> DataFolder=<폴더>\n" +
            "    프로그램 폴더와 Android 이미지, 설정, 로그를\n" +
            "    둘 폴더를 정합니다.");

        public static string InstallButton => Pick("Install", "설치");
        public static string CancelButton => Pick("Cancel", "취소");
        public static string BackButton => Pick("Back", "뒤로");
        public static string CloseButton => Pick("Close", "닫기");
        public static string DoneButton => Pick("Done", "완료");
        public static string OkButton => Pick("OK", "확인");
        public static string RepairButton => Pick("Repair", "복구");
        public static string UninstallButton => Pick("Uninstall", "제거");
        public static string RestartButton => Pick("Restart", "다시 시작");
        public static string Minimize => Pick("Minimize", "최소화");

        public static string ConfirmCancel => Pick("Stop setup?", "설치를 멈출까요?");
    }
}
