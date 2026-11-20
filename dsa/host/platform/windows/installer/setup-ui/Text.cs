using System.Globalization;
using Dsa.Settings.Core;

namespace Dsa.Setup
{
    /// <summary>The words on the setup screens, in the user's display language: one of DSA's
    /// fourteen interface languages, picked as the settings app picks it (Traditional Chinese
    /// for zh-Hant, zh-TW, zh-HK and zh-MO, Simplified for the rest of Chinese, Brazilian
    /// Portuguese for every Portuguese), and English for any other. Android, DSA, the product
    /// name and the commands are never translated.
    ///
    /// Each text is given in every language, in this order: English, Korean, Japanese,
    /// Simplified Chinese, Traditional Chinese, Spanish, French, German, Brazilian Portuguese,
    /// Russian, Italian, Vietnamese, Indonesian, Turkish.</summary>
    internal static class Text
    {
        /// <summary>The language the screens are in, from the user's display language.</summary>
        internal static UiLanguage Language { get; set; } = UiLanguage.Resolve(CultureInfo.CurrentUICulture);

        private static string Pick(string en, string ko, string ja, string zhCN, string zhTW, string es, string fr,
            string de, string ptBR, string ru, string it, string vi, string id, string tr)
        {
            var language = Language;
            if (language == UiLanguage.Korean) return ko;
            if (language == UiLanguage.Japanese) return ja;
            if (language == UiLanguage.ChineseSimplified) return zhCN;
            if (language == UiLanguage.ChineseTraditional) return zhTW;
            if (language == UiLanguage.Spanish) return es;
            if (language == UiLanguage.French) return fr;
            if (language == UiLanguage.German) return de;
            if (language == UiLanguage.PortugueseBrazil) return ptBR;
            if (language == UiLanguage.Russian) return ru;
            if (language == UiLanguage.Italian) return it;
            if (language == UiLanguage.Vietnamese) return vi;
            if (language == UiLanguage.Indonesian) return id;
            if (language == UiLanguage.Turkish) return tr;
            return en;
        }

        public static string Caption => Pick(
            "Desktop Subsystem for Android™ Setup",
            "Desktop Subsystem for Android™ 설치",
            "Desktop Subsystem for Android™ セットアップ",
            "Desktop Subsystem for Android™ 安装程序",
            "Desktop Subsystem for Android™ 安裝程式",
            "Instalación de Desktop Subsystem for Android™",
            "Installation de Desktop Subsystem for Android™",
            "Desktop Subsystem for Android™ Setup",
            "Instalação do Desktop Subsystem for Android™",
            "Установка Desktop Subsystem for Android™",
            "Installazione di Desktop Subsystem for Android™",
            "Cài đặt Desktop Subsystem for Android™",
            "Penyiapan Desktop Subsystem for Android™",
            "Desktop Subsystem for Android™ Kurulumu");
        public const string Title = "Desktop Subsystem for Android™";
        public const string Publisher = "ThisIsThePy";
        public const string Architecture = "x64";
        public static string VersionChip => Pick(
            "Version {0}",
            "버전 {0}",
            "バージョン {0}",
            "版本 {0}",
            "版本 {0}",
            "Versión {0}",
            "Version {0}",
            "Version {0}",
            "Versão {0}",
            "Версия {0}",
            "Versione {0}",
            "Phiên bản {0}",
            "Versi {0}",
            "Sürüm {0}");
        public const string AndroidChip = "Android {0}";
        /// <summary>The chip of a preset setup (a17, a16): the Android it installs and keeps.</summary>
        public static string AndroidChipPinned => Pick(
            "Android {0} (fixed)",
            "Android {0} 고정",
            "Android {0} (固定)",
            "Android {0}（固定）",
            "Android {0}（固定）",
            "Android {0} (fijo)",
            "Android {0} (fixe)",
            "Android {0} (fest)",
            "Android {0} (fixo)",
            "Android {0} (закреплено)",
            "Android {0} (fisso)",
            "Android {0} (cố định)",
            "Android {0} (tetap)",
            "Android {0} (sabit)");
        public static string Logo => Pick(
            "Desktop Subsystem for Android™ logo",
            "Desktop Subsystem for Android™ 로고",
            "Desktop Subsystem for Android™ のロゴ",
            "Desktop Subsystem for Android™ 徽标",
            "Desktop Subsystem for Android™ 標誌",
            "Logotipo de Desktop Subsystem for Android™",
            "Logo de Desktop Subsystem for Android™",
            "Logo von Desktop Subsystem for Android™",
            "Logotipo do Desktop Subsystem for Android™",
            "Логотип Desktop Subsystem for Android™",
            "Logo di Desktop Subsystem for Android™",
            "Biểu trưng Desktop Subsystem for Android™",
            "Logo Desktop Subsystem for Android™",
            "Desktop Subsystem for Android™ logosu");

        public static string InstallText => Pick(
            "Run Android apps in their own windows on your desktop. Setup installs the DSA service and downloads Android (about 1.8 GB).",
            "Android 앱을 데스크톱에서 각자의 창으로 실행합니다. 설치 프로그램이 DSA 서비스를 설치하고 Android를 내려받습니다(약 1.8GB).",
            "Android アプリをデスクトップ上でそれぞれのウィンドウで実行します。セットアップは DSA サービスをインストールし、Android をダウンロードします (約 1.8 GB)。",
            "在桌面上以各自独立的窗口运行 Android 应用。安装程序会安装 DSA 服务并下载 Android（约 1.8 GB）。",
            "在桌面上以各自的視窗執行 Android 應用程式。安裝程式會安裝 DSA 服務並下載 Android (約 1.8 GB)。",
            "Ejecute aplicaciones de Android en sus propias ventanas en el escritorio. El programa de instalación instala el servicio DSA y descarga Android (unos 1,8 GB).",
            "Exécutez les applications Android dans leurs propres fenêtres sur votre bureau. Le programme d'installation installe le service DSA et télécharge Android (environ 1,8 Go).",
            "Führen Sie Android-Apps in eigenen Fenstern auf Ihrem Desktop aus. Das Setup installiert den DSA-Dienst und lädt Android herunter (etwa 1,8 GB).",
            "Execute apps Android em janelas próprias na sua área de trabalho. A instalação instala o serviço do DSA e baixa o Android (cerca de 1,8 GB).",
            "Запускайте приложения Android в отдельных окнах на рабочем столе. Программа установки устанавливает службу DSA и скачивает Android (около 1,8 ГБ).",
            "Esegui le app Android nelle loro finestre sul desktop. Il programma di installazione installa il servizio DSA e scarica Android (circa 1,8 GB).",
            "Chạy ứng dụng Android trong cửa sổ riêng trên màn hình nền. Trình cài đặt sẽ cài dịch vụ DSA và tải xuống Android (khoảng 1,8 GB).",
            "Jalankan aplikasi Android di jendelanya sendiri di desktop Anda. Penyiapan memasang layanan DSA dan mengunduh Android (sekitar 1,8 GB).",
            "Android uygulamalarını masaüstünüzde kendi pencerelerinde çalıştırın. Kurulum, DSA hizmetini yükler ve Android'i indirir (yaklaşık 1,8 GB).");
        public static string InstallTextBundledImage => Pick(
            "Run Android apps in their own windows on your desktop. Setup installs the DSA service and Android.",
            "Android 앱을 데스크톱에서 각자의 창으로 실행합니다. 설치 프로그램이 DSA 서비스와 Android를 설치합니다.",
            "Android アプリをデスクトップ上でそれぞれのウィンドウで実行します。セットアップは DSA サービスと Android をインストールします。",
            "在桌面上以各自独立的窗口运行 Android 应用。安装程序会安装 DSA 服务和 Android。",
            "在桌面上以各自的視窗執行 Android 應用程式。安裝程式會安裝 DSA 服務和 Android。",
            "Ejecute aplicaciones de Android en sus propias ventanas en el escritorio. El programa de instalación instala el servicio DSA y Android.",
            "Exécutez les applications Android dans leurs propres fenêtres sur votre bureau. Le programme d'installation installe le service DSA et Android.",
            "Führen Sie Android-Apps in eigenen Fenstern auf Ihrem Desktop aus. Das Setup installiert den DSA-Dienst und Android.",
            "Execute apps Android em janelas próprias na sua área de trabalho. A instalação instala o serviço do DSA e o Android.",
            "Запускайте приложения Android в отдельных окнах на рабочем столе. Программа установки устанавливает службу DSA и Android.",
            "Esegui le app Android nelle loro finestre sul desktop. Il programma di installazione installa il servizio DSA e Android.",
            "Chạy ứng dụng Android trong cửa sổ riêng trên màn hình nền. Trình cài đặt sẽ cài dịch vụ DSA và Android.",
            "Jalankan aplikasi Android di jendelanya sendiri di desktop Anda. Penyiapan memasang layanan DSA dan Android.",
            "Android uygulamalarını masaüstünüzde kendi pencerelerinde çalıştırın. Kurulum, DSA hizmetini ve Android'i yükler.");
        public static string LocationHeader => Pick(
            "Install location",
            "설치 위치",
            "インストール先",
            "安装位置",
            "安裝位置",
            "Ubicación de instalación",
            "Emplacement d'installation",
            "Installationsort",
            "Local de instalação",
            "Расположение установки",
            "Percorso di installazione",
            "Vị trí cài đặt",
            "Lokasi pemasangan",
            "Yükleme konumu");
        public static string LocationData => Pick(
            "Android images in {0}",
            "Android 이미지 위치: {0}",
            "Android イメージの場所: {0}",
            "Android 映像位置：{0}",
            "Android 映像位置：{0}",
            "Imágenes de Android en {0}",
            "Images Android dans {0}",
            "Android-Images in {0}",
            "Imagens do Android em {0}",
            "Образы Android: {0}",
            "Immagini di Android in {0}",
            "Ảnh hệ thống Android tại {0}",
            "Image Android di {0}",
            "Android görüntüleri: {0}");
        public static string ChangeButton => Pick(
            "Change",
            "변경",
            "変更",
            "更改",
            "變更",
            "Cambiar",
            "Modifier",
            "Ändern",
            "Alterar",
            "Изменить",
            "Cambia",
            "Thay đổi",
            "Ubah",
            "Değiştir");
        public static string LegalBefore => Pick(
            "By installing, you agree to the ",
            "설치하면 ",
            "インストールすると、",
            "安装即表示你同意 ",
            "安裝即表示您同意 ",
            "Al instalar, acepta la ",
            "En installant, vous acceptez la ",
            "Mit der Installation stimmen Sie der ",
            "Ao instalar, você concorda com a ",
            "Устанавливая программу, вы принимаете условия ",
            "Installando, accetti la ",
            "Khi cài đặt, bạn đồng ý với ",
            "Dengan memasang, Anda menyetujui ",
            "Yükleyerek ");
        public const string LegalLink = "Apache License 2.0";
        public static string LegalAfter => Pick(
            ". DSA is an independent open-source project. It is not affiliated with, " +
            "endorsed by or sponsored by Microsoft or Google. Android is a trademark of Google LLC.",
            "에 동의하게 됩니다. DSA는 독립 오픈 소스 프로젝트이며 Microsoft나 Google과 " +
            "제휴하거나 그 보증이나 후원을 받지 않습니다. Android는 Google LLC의 상표입니다.",
            " に同意したことになります。DSA は独立したオープンソース プロジェクトです。" +
            "Microsoft および Google とは提携しておらず、両社による承認や後援も受けていません。Android は Google LLC の商標です。",
            "。DSA 是一个独立的开源项目，与 Microsoft 或 Google 没有关联，也未获得二者的认可或赞助。" +
            "Android 是 Google LLC 的商标。",
            "。DSA 是獨立的開放原始碼專案，與 Microsoft 或 Google 沒有任何關聯，也未獲得其背書或贊助。" +
            "Android 是 Google LLC 的商標。",
            ". DSA es un proyecto independiente de código abierto. No está afiliado a Microsoft ni a Google, " +
            "ni cuenta con su respaldo o patrocinio. Android es una marca comercial de Google LLC.",
            ". DSA est un projet open source indépendant. Il n'est ni affilié à Microsoft ou à Google, " +
            "ni approuvé ou parrainé par eux. Android est une marque de Google LLC.",
            " zu. DSA ist ein unabhängiges Open-Source-Projekt. Es steht in keiner Verbindung zu Microsoft " +
            "oder Google und wird von ihnen weder unterstützt noch gesponsert. Android ist eine Marke von Google LLC.",
            ". O DSA é um projeto de código aberto independente. Ele não é afiliado à Microsoft nem ao Google, " +
            "nem endossado ou patrocinado por eles. Android é uma marca registrada da Google LLC.",
            ". DSA является независимым проектом с открытым исходным кодом. Он не связан с Microsoft или Google, " +
            "не одобрен и не спонсируется ими. Android является товарным знаком Google LLC.",
            ". DSA è un progetto open source indipendente. Non è affiliato a Microsoft o Google, " +
            "né approvato o sponsorizzato da loro. Android è un marchio di Google LLC.",
            ". DSA là một dự án mã nguồn mở độc lập. DSA không liên kết với Microsoft hay Google, " +
            "và không được hai công ty này xác nhận hoặc tài trợ. Android là nhãn hiệu của Google LLC.",
            ". DSA adalah proyek sumber terbuka yang independen. DSA tidak berafiliasi dengan, tidak didukung oleh, " +
            "dan tidak disponsori oleh Microsoft maupun Google. Android adalah merek dagang Google LLC.",
            "'ı kabul etmiş olursunuz. DSA bağımsız bir açık kaynak projesidir. Microsoft veya Google ile bağlantılı " +
            "değildir, onlar tarafından onaylanmamış ve desteklenmemektedir. Android, Google LLC'nin ticari markasıdır.");
        public const string LicenseUrl = "https://www.apache.org/licenses/LICENSE-2.0";

        public static string OptionsHeader => LocationHeader;
        public static string OptionsProgramLabel => Pick(
            "Program folder",
            "프로그램 폴더",
            "プログラム フォルダー",
            "程序文件夹",
            "程式資料夾",
            "Carpeta del programa",
            "Dossier du programme",
            "Programmordner",
            "Pasta do programa",
            "Папка программы",
            "Cartella del programma",
            "Thư mục chương trình",
            "Folder program",
            "Program klasörü");
        public static string OptionsDataLabel => Pick(
            "Folder for the Android images, settings and logs",
            "Android 이미지, 설정, 로그를 둘 폴더",
            "Android のイメージ、設定、ログを置くフォルダー",
            "存放 Android 映像、设置和日志的文件夹",
            "存放 Android 映像、設定和記錄的資料夾",
            "Carpeta para las imágenes, la configuración y los registros de Android",
            "Dossier des images, des paramètres et des journaux d'Android",
            "Ordner für die Android-Images, Einstellungen und Protokolle",
            "Pasta para as imagens, configurações e logs do Android",
            "Папка для образов, параметров и журналов Android",
            "Cartella per le immagini, le impostazioni e i log di Android",
            "Thư mục chứa ảnh hệ thống, cài đặt và nhật ký của Android",
            "Folder untuk image, pengaturan, dan log Android",
            "Android görüntüleri, ayarları ve günlükleri için klasör");
        public static string OptionsDataNote => Pick(
            "Each user's Android data is kept in their own profile, under AppData\\Local\\DSA.",
            "사용자마다 Android 데이터는 각자의 프로필 아래 AppData\\Local\\DSA에 보관됩니다.",
            "各ユーザーの Android データは、そのユーザーのプロファイルの AppData\\Local\\DSA に保存されます。",
            "每个用户的 Android 数据保存在其个人配置文件的 AppData\\Local\\DSA 下。",
            "每位使用者的 Android 資料保存在其設定檔的 AppData\\Local\\DSA 下。",
            "Los datos de Android de cada usuario se guardan en su propio perfil, en AppData\\Local\\DSA.",
            "Les données Android de chaque utilisateur sont conservées dans son propre profil, sous AppData\\Local\\DSA.",
            "Die Android-Daten jedes Benutzers liegen in seinem eigenen Profil unter AppData\\Local\\DSA.",
            "Os dados do Android de cada usuário ficam no próprio perfil, em AppData\\Local\\DSA.",
            "Данные Android каждого пользователя хранятся в его собственном профиле, в папке AppData\\Local\\DSA.",
            "I dati di Android di ogni utente sono conservati nel suo profilo, in AppData\\Local\\DSA.",
            "Dữ liệu Android của từng người dùng được lưu trong hồ sơ riêng của họ, tại AppData\\Local\\DSA.",
            "Data Android setiap pengguna disimpan di profilnya sendiri, di AppData\\Local\\DSA.",
            "Her kullanıcının Android verileri kendi profilinde, AppData\\Local\\DSA altında tutulur.");
        public static string OptionsInvalid => Pick(
            "Choose a full path, such as C:\\Program Files\\DSA.",
            "C:\\Program Files\\DSA처럼 전체 경로를 고르세요.",
            "C:\\Program Files\\DSA のような完全なパスを選んでください。",
            "请选择完整路径，例如 C:\\Program Files\\DSA。",
            "請選擇完整路徑，例如 C:\\Program Files\\DSA。",
            "Elija una ruta completa, como C:\\Program Files\\DSA.",
            "Choisissez un chemin complet, par exemple C:\\Program Files\\DSA.",
            "Wählen Sie einen vollständigen Pfad, etwa C:\\Program Files\\DSA.",
            "Escolha um caminho completo, como C:\\Program Files\\DSA.",
            "Выберите полный путь, например C:\\Program Files\\DSA.",
            "Scegli un percorso completo, ad esempio C:\\Program Files\\DSA.",
            "Hãy chọn đường dẫn đầy đủ, chẳng hạn C:\\Program Files\\DSA.",
            "Pilih jalur lengkap, misalnya C:\\Program Files\\DSA.",
            "C:\\Program Files\\DSA gibi tam bir yol seçin.");
        public static string BrowseButton => Pick(
            "Browse",
            "찾아보기",
            "参照",
            "浏览",
            "瀏覽",
            "Examinar",
            "Parcourir",
            "Durchsuchen",
            "Procurar",
            "Обзор",
            "Sfoglia",
            "Duyệt",
            "Telusuri",
            "Gözat");
        public static string BrowseProgramTitle => Pick(
            "Choose the program folder",
            "프로그램 폴더 선택",
            "プログラム フォルダーの選択",
            "选择程序文件夹",
            "選擇程式資料夾",
            "Elija la carpeta del programa",
            "Choisissez le dossier du programme",
            "Programmordner auswählen",
            "Escolha a pasta do programa",
            "Выберите папку программы",
            "Scegli la cartella del programma",
            "Chọn thư mục chương trình",
            "Pilih folder program",
            "Program klasörünü seçin");
        public static string BrowseDataTitle => Pick(
            "Choose the folder for the Android images, settings and logs",
            "Android 이미지, 설정, 로그를 둘 폴더 선택",
            "Android のイメージ、設定、ログを置くフォルダーの選択",
            "选择存放 Android 映像、设置和日志的文件夹",
            "選擇存放 Android 映像、設定和記錄的資料夾",
            "Elija la carpeta para las imágenes, la configuración y los registros de Android",
            "Choisissez le dossier des images, des paramètres et des journaux d'Android",
            "Ordner für die Android-Images, Einstellungen und Protokolle auswählen",
            "Escolha a pasta para as imagens, configurações e logs do Android",
            "Выберите папку для образов, параметров и журналов Android",
            "Scegli la cartella per le immagini, le impostazioni e i log di Android",
            "Chọn thư mục chứa ảnh hệ thống, cài đặt và nhật ký của Android",
            "Pilih folder untuk image, pengaturan, dan log Android",
            "Android görüntüleri, ayarları ve günlükleri için klasörü seçin");

        public static string ModifyText => Pick(
            "Desktop Subsystem for Android is installed. Repair puts back missing or damaged files. Uninstall removes the program and the downloaded Android images, and keeps each user's Android data.",
            "Desktop Subsystem for Android가 설치되어 있습니다. 복구하면 없어지거나 손상된 파일을 되돌립니다. 제거하면 프로그램과 내려받은 Android 이미지를 지우고, 사용자마다의 Android 데이터는 남깁니다.",
            "Desktop Subsystem for Android はインストールされています。[修復] は、見つからないファイルや破損したファイルを元に戻します。[アンインストール] は、プログラムとダウンロードした Android イメージを削除し、各ユーザーの Android データは残します。",
            "Desktop Subsystem for Android 已安装。“修复”会恢复缺失或损坏的文件。“卸载”会删除程序和已下载的 Android 映像，并保留每个用户的 Android 数据。",
            "Desktop Subsystem for Android 已安裝。「修復」會還原遺失或損毀的檔案。「解除安裝」會移除程式和已下載的 Android 映像，並保留每位使用者的 Android 資料。",
            "Desktop Subsystem for Android está instalado. Reparar restaura los archivos que faltan o están dañados. Desinstalar quita el programa y las imágenes de Android descargadas, y conserva los datos de Android de cada usuario.",
            "Desktop Subsystem for Android est installé. Réparer remet en place les fichiers manquants ou endommagés. Désinstaller supprime le programme et les images Android téléchargées, et conserve les données Android de chaque utilisateur.",
            "Desktop Subsystem for Android ist installiert. „Reparieren“ stellt fehlende oder beschädigte Dateien wieder her. „Deinstallieren“ entfernt das Programm und die heruntergeladenen Android-Images und behält die Android-Daten jedes Benutzers.",
            "O Desktop Subsystem for Android está instalado. Reparar restaura arquivos ausentes ou danificados. Desinstalar remove o programa e as imagens do Android baixadas, e mantém os dados do Android de cada usuário.",
            "Desktop Subsystem for Android установлен. «Восстановить» возвращает отсутствующие или повреждённые файлы. «Удалить» удаляет программу и скачанные образы Android, но сохраняет данные Android каждого пользователя.",
            "Desktop Subsystem for Android è installato. Ripara ripristina i file mancanti o danneggiati. Disinstalla rimuove il programma e le immagini di Android scaricate, e mantiene i dati di Android di ogni utente.",
            "Desktop Subsystem for Android đã được cài đặt. Sửa chữa sẽ khôi phục các tệp bị thiếu hoặc bị hỏng. Gỡ cài đặt sẽ xóa chương trình và các ảnh hệ thống Android đã tải xuống, nhưng giữ lại dữ liệu Android của từng người dùng.",
            "Desktop Subsystem for Android sudah terpasang. Perbaiki mengembalikan file yang hilang atau rusak. Copot pemasangan menghapus program dan image Android yang diunduh, serta menyimpan data Android setiap pengguna.",
            "Desktop Subsystem for Android yüklü. Onar, eksik veya hasarlı dosyaları geri getirir. Kaldır, programı ve indirilen Android görüntülerini kaldırır; her kullanıcının Android verilerini korur.");
        public static string InstalledLocation => Pick(
            "Installed in {0}",
            "설치 위치: {0}",
            "インストール先: {0}",
            "安装位置：{0}",
            "安裝位置：{0}",
            "Instalado en {0}",
            "Installé dans {0}",
            "Installiert in {0}",
            "Instalado em {0}",
            "Установлено в {0}",
            "Installato in {0}",
            "Đã cài đặt tại {0}",
            "Terpasang di {0}",
            "Yükleme konumu: {0}");

        public static string ProgressInstall => Pick(
            "Installing",
            "설치하는 중",
            "インストールしています",
            "正在安装",
            "正在安裝",
            "Instalando",
            "Installation en cours",
            "Wird installiert",
            "Instalando",
            "Установка",
            "Installazione in corso",
            "Đang cài đặt",
            "Memasang",
            "Yükleniyor");
        public static string ProgressRepair => Pick(
            "Repairing",
            "복구하는 중",
            "修復しています",
            "正在修复",
            "正在修復",
            "Reparando",
            "Réparation en cours",
            "Wird repariert",
            "Reparando",
            "Восстановление",
            "Riparazione in corso",
            "Đang sửa chữa",
            "Memperbaiki",
            "Onarılıyor");
        public static string ProgressUninstall => Pick(
            "Uninstalling",
            "제거하는 중",
            "アンインストールしています",
            "正在卸载",
            "正在解除安裝",
            "Desinstalando",
            "Désinstallation en cours",
            "Wird deinstalliert",
            "Desinstalando",
            "Удаление",
            "Disinstallazione in corso",
            "Đang gỡ cài đặt",
            "Mencopot pemasangan",
            "Kaldırılıyor");
        public static string ProgressLayout => Pick(
            "Copying the setup files",
            "설치 파일을 복사하는 중",
            "セットアップ ファイルをコピーしています",
            "正在复制安装文件",
            "正在複製安裝檔案",
            "Copiando los archivos de instalación",
            "Copie des fichiers d'installation",
            "Setupdateien werden kopiert",
            "Copiando os arquivos de instalação",
            "Копирование файлов установки",
            "Copia dei file di installazione",
            "Đang sao chép tệp cài đặt",
            "Menyalin file penyiapan",
            "Kurulum dosyaları kopyalanıyor");
        public static string ProgressPreparing => Pick(
            "Preparing…",
            "준비하는 중…",
            "準備しています…",
            "正在准备…",
            "正在準備…",
            "Preparando…",
            "Préparation…",
            "Wird vorbereitet…",
            "Preparando…",
            "Подготовка…",
            "Preparazione…",
            "Đang chuẩn bị…",
            "Menyiapkan…",
            "Hazırlanıyor…");
        public static string ProgressPreviousVersion => Pick(
            "Removing the previous version…",
            "이전 버전을 제거하는 중…",
            "以前のバージョンを削除しています…",
            "正在删除旧版本…",
            "正在移除舊版本…",
            "Quitando la versión anterior…",
            "Suppression de la version précédente…",
            "Vorherige Version wird entfernt…",
            "Removendo a versão anterior…",
            "Удаление предыдущей версии…",
            "Rimozione della versione precedente…",
            "Đang gỡ phiên bản trước…",
            "Menghapus versi sebelumnya…",
            "Önceki sürüm kaldırılıyor…");
        public static string ProgressCancelling => Pick(
            "Cancelling…",
            "취소하는 중…",
            "キャンセルしています…",
            "正在取消…",
            "正在取消…",
            "Cancelando…",
            "Annulation…",
            "Wird abgebrochen…",
            "Cancelando…",
            "Отмена…",
            "Annullamento…",
            "Đang hủy…",
            "Membatalkan…",
            "İptal ediliyor…");

        public static string SuccessInstall => Pick(
            "Installed",
            "설치됨",
            "インストールしました",
            "已安装",
            "已安裝",
            "Instalado",
            "Installé",
            "Installiert",
            "Instalado",
            "Установлено",
            "Installato",
            "Đã cài đặt",
            "Terpasang",
            "Yüklendi");
        public static string SuccessInstallText => Pick(
            "Android is downloading in the background. Type dsa status in a terminal to follow it.",
            "Android를 백그라운드에서 내려받고 있습니다. 진행 상황은 터미널에서 dsa status를 입력해 확인하세요.",
            "Android をバックグラウンドでダウンロードしています。進行状況はターミナルで dsa status と入力して確認してください。",
            "Android 正在后台下载。在终端中输入 dsa status 可查看进度。",
            "Android 正在背景下載。在終端機中輸入 dsa status 即可查看進度。",
            "Android se está descargando en segundo plano. Escriba dsa status en un terminal para seguir el progreso.",
            "Android se télécharge en arrière-plan. Tapez dsa status dans un terminal pour suivre la progression.",
            "Android wird im Hintergrund heruntergeladen. Geben Sie dsa status in einem Terminal ein, um den Fortschritt zu verfolgen.",
            "O Android está sendo baixado em segundo plano. Digite dsa status em um terminal para acompanhar.",
            "Android скачивается в фоновом режиме. Чтобы следить за ходом, введите dsa status в терминале.",
            "Android viene scaricato in background. Digita dsa status in un terminale per seguire l'avanzamento.",
            "Android đang được tải xuống trong nền. Nhập dsa status trong cửa sổ dòng lệnh để theo dõi.",
            "Android sedang diunduh di latar belakang. Ketik dsa status di terminal untuk memantaunya.",
            "Android arka planda indiriliyor. İlerlemeyi izlemek için bir terminale dsa status yazın.");
        public static string SuccessInstallTextBundledImage => Pick(
            "The DSA service is running. Type dsa status in a terminal to see your Android.",
            "DSA 서비스가 실행 중입니다. 내 Android 상태는 터미널에서 dsa status를 입력해 확인하세요.",
            "DSA サービスが実行中です。Android の状態はターミナルで dsa status と入力して確認してください。",
            "DSA 服务正在运行。在终端中输入 dsa status 可查看你的 Android。",
            "DSA 服務正在執行。在終端機中輸入 dsa status 即可查看您的 Android。",
            "El servicio DSA se está ejecutando. Escriba dsa status en un terminal para ver su Android.",
            "Le service DSA est en cours d'exécution. Tapez dsa status dans un terminal pour voir votre Android.",
            "Der DSA-Dienst wird ausgeführt. Geben Sie dsa status in einem Terminal ein, um Ihr Android zu sehen.",
            "O serviço do DSA está em execução. Digite dsa status em um terminal para ver seu Android.",
            "Служба DSA запущена. Чтобы увидеть состояние Android, введите dsa status в терминале.",
            "Il servizio DSA è in esecuzione. Digita dsa status in un terminale per vedere il tuo Android.",
            "Dịch vụ DSA đang chạy. Nhập dsa status trong cửa sổ dòng lệnh để xem Android của bạn.",
            "Layanan DSA sedang berjalan. Ketik dsa status di terminal untuk melihat Android Anda.",
            "DSA hizmeti çalışıyor. Android'inizi görmek için bir terminale dsa status yazın.");
        public static string SuccessRepair => Pick(
            "Repaired",
            "복구됨",
            "修復しました",
            "已修复",
            "已修復",
            "Reparado",
            "Réparé",
            "Repariert",
            "Reparado",
            "Восстановлено",
            "Riparato",
            "Đã sửa chữa",
            "Diperbaiki",
            "Onarıldı");
        public static string SuccessUninstall => Pick(
            "Uninstalled",
            "제거됨",
            "アンインストールしました",
            "已卸载",
            "已解除安裝",
            "Desinstalado",
            "Désinstallé",
            "Deinstalliert",
            "Desinstalado",
            "Удалено",
            "Disinstallato",
            "Đã gỡ cài đặt",
            "Pemasangan dicopot",
            "Kaldırıldı");
        public static string SuccessUninstallText => Pick(
            "Each user's Android data is kept in their profile, under AppData\\Local\\DSA, until they delete it.",
            "사용자마다 Android 데이터는 직접 지울 때까지 각자의 프로필 아래 AppData\\Local\\DSA에 남습니다.",
            "各ユーザーの Android データは、本人が削除するまでそのユーザーのプロファイルの AppData\\Local\\DSA に残ります。",
            "每个用户的 Android 数据会保留在其个人配置文件的 AppData\\Local\\DSA 下，直到用户自行删除。",
            "每位使用者的 Android 資料會保留在其設定檔的 AppData\\Local\\DSA 下，直到使用者自行刪除。",
            "Los datos de Android de cada usuario se conservan en su perfil, en AppData\\Local\\DSA, hasta que los elimine.",
            "Les données Android de chaque utilisateur restent dans son profil, sous AppData\\Local\\DSA, jusqu'à ce qu'il les supprime.",
            "Die Android-Daten jedes Benutzers bleiben in seinem Profil unter AppData\\Local\\DSA, bis er sie löscht.",
            "Os dados do Android de cada usuário ficam no perfil dele, em AppData\\Local\\DSA, até que ele os exclua.",
            "Данные Android каждого пользователя остаются в его профиле, в папке AppData\\Local\\DSA, пока он их не удалит.",
            "I dati di Android di ogni utente restano nel suo profilo, in AppData\\Local\\DSA, finché non li elimina.",
            "Dữ liệu Android của từng người dùng được giữ trong hồ sơ của họ, tại AppData\\Local\\DSA, cho đến khi họ xóa.",
            "Data Android setiap pengguna tetap disimpan di profilnya, di AppData\\Local\\DSA, sampai pengguna menghapusnya.",
            "Her kullanıcının Android verileri, kendisi silene kadar profilinde AppData\\Local\\DSA altında kalır.");
        public static string SuccessLayout => Pick(
            "Setup files copied",
            "설치 파일 복사됨",
            "セットアップ ファイルをコピーしました",
            "已复制安装文件",
            "已複製安裝檔案",
            "Archivos de instalación copiados",
            "Fichiers d'installation copiés",
            "Setupdateien kopiert",
            "Arquivos de instalação copiados",
            "Файлы установки скопированы",
            "File di installazione copiati",
            "Đã sao chép tệp cài đặt",
            "File penyiapan disalin",
            "Kurulum dosyaları kopyalandı");
        public static string SuccessLayoutText => Pick(
            "The setup files are in {0}.",
            "설치 파일 위치: {0}",
            "セットアップ ファイルの場所: {0}",
            "安装文件位于 {0}。",
            "安裝檔案位於 {0}。",
            "Los archivos de instalación están en {0}.",
            "Les fichiers d'installation se trouvent dans {0}.",
            "Die Setupdateien befinden sich in {0}.",
            "Os arquivos de instalação estão em {0}.",
            "Файлы установки находятся в {0}.",
            "I file di installazione si trovano in {0}.",
            "Các tệp cài đặt nằm trong {0}.",
            "File penyiapan ada di {0}.",
            "Kurulum dosyalarının konumu: {0}");
        public const string CodeInText = "dsa status";

        public static string RestartText => Pick(
            "Restart the computer to finish.",
            "마치려면 컴퓨터를 다시 시작하세요.",
            "完了するには、コンピューターを再起動してください。",
            "请重启计算机以完成操作。",
            "請重新啟動電腦以完成作業。",
            "Reinicie el equipo para terminar.",
            "Redémarrez l'ordinateur pour terminer.",
            "Starten Sie den Computer neu, um den Vorgang abzuschließen.",
            "Reinicie o computador para concluir.",
            "Чтобы завершить, перезагрузите компьютер.",
            "Riavvia il computer per completare.",
            "Hãy khởi động lại máy tính để hoàn tất.",
            "Mulai ulang komputer untuk menyelesaikan.",
            "Tamamlamak için bilgisayarı yeniden başlatın.");

        public static string FailureInstall => Pick(
            "Setup did not finish",
            "설치를 마치지 못했습니다",
            "セットアップを完了できませんでした",
            "安装未完成",
            "安裝未完成",
            "La instalación no terminó",
            "L'installation n'a pas abouti",
            "Das Setup wurde nicht abgeschlossen",
            "A instalação não foi concluída",
            "Установка не завершена",
            "L'installazione non è stata completata",
            "Cài đặt chưa hoàn tất",
            "Penyiapan tidak selesai",
            "Kurulum tamamlanmadı");
        public static string FailureUninstall => Pick(
            "Uninstall did not finish",
            "제거를 마치지 못했습니다",
            "アンインストールを完了できませんでした",
            "卸载未完成",
            "解除安裝未完成",
            "La desinstalación no terminó",
            "La désinstallation n'a pas abouti",
            "Die Deinstallation wurde nicht abgeschlossen",
            "A desinstalação não foi concluída",
            "Удаление не завершено",
            "La disinstallazione non è stata completata",
            "Gỡ cài đặt chưa hoàn tất",
            "Pencopotan pemasangan tidak selesai",
            "Kaldırma tamamlanmadı");
        public static string FailureCancelled => Pick(
            "You cancelled setup. It undid what it had done.",
            "설치를 취소했습니다. 그때까지 한 일은 되돌렸습니다.",
            "セットアップをキャンセルしました。それまでに行った変更は元に戻しました。",
            "你已取消安装。安装程序已撤消它所做的更改。",
            "您已取消安裝。安裝程式已復原它所做的變更。",
            "Canceló la instalación. Se deshizo lo que se había hecho.",
            "Vous avez annulé l'installation. Ce qu'elle avait fait a été annulé.",
            "Sie haben das Setup abgebrochen. Was es bereits getan hatte, wurde rückgängig gemacht.",
            "Você cancelou a instalação. O que ela já tinha feito foi desfeito.",
            "Вы отменили установку. Уже сделанные изменения отменены.",
            "Hai annullato l'installazione. Quanto era già stato fatto è stato annullato.",
            "Bạn đã hủy cài đặt. Những gì đã thực hiện đã được hoàn tác.",
            "Anda membatalkan penyiapan. Apa yang sudah dilakukan telah dibatalkan.",
            "Kurulumu iptal ettiniz. O ana kadar yapılanlar geri alındı.");
        // The sentence around the log link: what comes before it, the link, and what comes after.
        // A language whose sentence starts with the link has nothing before it.
        public static string FailureLogBefore => Pick(
            "The ",
            "",
            "",
            "请查看",
            "請查看",
            "El ",
            "Le ",
            "Die ",
            "O ",
            "Причина ошибки указана в ",
            "Il ",
            "",
            "",
            "Neyin yanlış gittiğini ");
        public static string FailureLogLink => Pick(
            "log file",
            "로그 파일",
            "ログ ファイル",
            "日志文件",
            "記錄檔",
            "archivo de registro",
            "fichier journal",
            "Protokolldatei",
            "arquivo de log",
            "файле журнала",
            "file di log",
            "Tệp nhật ký",
            "File log",
            "günlük dosyası");
        public static string FailureLogAfter => Pick(
            " says what went wrong.",
            "에서 무엇이 잘못됐는지 볼 수 있습니다.",
            "で、何が問題だったかを確認できます。",
            "了解出了什么问题。",
            "以了解發生了什麼問題。",
            " indica qué salió mal.",
            " indique ce qui s'est mal passé.",
            " zeigt, was schiefgelaufen ist.",
            " informa o que deu errado.",
            ".",
            " indica cosa è andato storto.",
            " cho biết đã xảy ra lỗi gì.",
            " menjelaskan apa yang salah.",
            " gösterir.");

        public static string NewerHeader => Pick(
            "A newer version is installed",
            "더 새 버전이 설치되어 있습니다",
            "新しいバージョンがインストールされています",
            "已安装更新的版本",
            "已安裝較新的版本",
            "Hay una versión más reciente instalada",
            "Une version plus récente est installée",
            "Eine neuere Version ist installiert",
            "Uma versão mais recente está instalada",
            "Установлена более новая версия",
            "È installata una versione più recente",
            "Đã cài đặt phiên bản mới hơn",
            "Versi yang lebih baru sudah terpasang",
            "Daha yeni bir sürüm yüklü");
        public static string NewerText => Pick(
            "A newer Desktop Subsystem for Android is already on this computer, so setup has nothing to do.",
            "이 컴퓨터에 더 새 Desktop Subsystem for Android가 이미 있어 설치할 것이 없습니다.",
            "このコンピューターには新しい Desktop Subsystem for Android が既にあるため、セットアップで行うことはありません。",
            "这台计算机上已有更新的 Desktop Subsystem for Android，因此安装程序无需执行任何操作。",
            "這台電腦上已有較新的 Desktop Subsystem for Android，因此安裝程式不需要執行任何動作。",
            "Este equipo ya tiene una versión más reciente de Desktop Subsystem for Android, así que la instalación no tiene nada que hacer.",
            "Une version plus récente de Desktop Subsystem for Android est déjà présente sur cet ordinateur, l'installation n'a donc rien à faire.",
            "Auf diesem Computer ist bereits eine neuere Version von Desktop Subsystem for Android vorhanden, daher hat das Setup nichts zu tun.",
            "Este computador já tem uma versão mais recente do Desktop Subsystem for Android, então a instalação não tem nada a fazer.",
            "На этом компьютере уже есть более новая версия Desktop Subsystem for Android, поэтому установке нечего делать.",
            "Su questo computer è già presente una versione più recente di Desktop Subsystem for Android, quindi l'installazione non ha nulla da fare.",
            "Máy tính này đã có phiên bản Desktop Subsystem for Android mới hơn, nên trình cài đặt không cần làm gì.",
            "Komputer ini sudah memiliki Desktop Subsystem for Android yang lebih baru, jadi penyiapan tidak perlu melakukan apa pun.",
            "Bu bilgisayarda zaten daha yeni bir Desktop Subsystem for Android var, bu yüzden kurulumun yapacağı bir şey yok.");

        public static string HelpHeader => Pick(
            "Setup help",
            "설치 도움말",
            "セットアップのヘルプ",
            "安装程序帮助",
            "安裝程式說明",
            "Ayuda de la instalación",
            "Aide de l'installation",
            "Setup-Hilfe",
            "Ajuda da instalação",
            "Справка по установке",
            "Guida all'installazione",
            "Trợ giúp cài đặt",
            "Bantuan penyiapan",
            "Kurulum yardımı");
        // The switches start each entry at the left, and what one does follows indented by four
        // spaces, on as many lines in every language as in English. The help is set in a
        // fixed-width font, so the columns line up.
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
            "    둘 폴더를 정합니다.",
            "/install | /repair | /uninstall | /layout [フォルダー]\n" +
            "    インストール、修復、アンインストールを行うか、\n" +
            "    セットアップ ファイルをフォルダーにコピーします。\n" +
            "/passive | /quiet\n" +
            "    進行状況だけを表示するか、何も表示しません。\n" +
            "/norestart\n" +
            "    コンピューターを再起動しません。\n" +
            "/log log.txt\n" +
            "    ログをファイルに書き込みます。\n" +
            "InstallFolder=<フォルダー> DataFolder=<フォルダー>\n" +
            "    プログラム フォルダーと、Android のイメージ、\n" +
            "    設定、ログを置くフォルダーを指定します。",
            "/install | /repair | /uninstall | /layout [文件夹]\n" +
            "    安装、修复、卸载，或将安装文件复制到\n" +
            "    文件夹。\n" +
            "/passive | /quiet\n" +
            "    只显示进度，或完全不显示。\n" +
            "/norestart\n" +
            "    从不重启计算机。\n" +
            "/log log.txt\n" +
            "    将日志写入该文件。\n" +
            "InstallFolder=<文件夹> DataFolder=<文件夹>\n" +
            "    设置程序文件夹，以及存放 Android 映像、\n" +
            "    设置和日志的文件夹。",
            "/install | /repair | /uninstall | /layout [資料夾]\n" +
            "    安裝、修復、解除安裝，或將安裝檔案複製到\n" +
            "    資料夾。\n" +
            "/passive | /quiet\n" +
            "    只顯示進度，或完全不顯示。\n" +
            "/norestart\n" +
            "    永不重新啟動電腦。\n" +
            "/log log.txt\n" +
            "    將記錄寫入該檔案。\n" +
            "InstallFolder=<資料夾> DataFolder=<資料夾>\n" +
            "    設定程式資料夾，以及存放 Android 映像、\n" +
            "    設定和記錄的資料夾。",
            "/install | /repair | /uninstall | /layout [carpeta]\n" +
            "    instala, repara, desinstala o copia los archivos\n" +
            "    de instalación en una carpeta.\n" +
            "/passive | /quiet\n" +
            "    muestra solo el progreso, o nada en absoluto.\n" +
            "/norestart\n" +
            "    nunca reinicia el equipo.\n" +
            "/log log.txt\n" +
            "    escribe un registro en el archivo.\n" +
            "InstallFolder=<carpeta> DataFolder=<carpeta>\n" +
            "    establece la carpeta del programa y la carpeta\n" +
            "    de imágenes, configuración y registros de Android.",
            "/install | /repair | /uninstall | /layout [dossier]\n" +
            "    installe, répare, désinstalle ou copie les\n" +
            "    fichiers d'installation dans un dossier.\n" +
            "/passive | /quiet\n" +
            "    n'affiche que la progression, ou rien du tout.\n" +
            "/norestart\n" +
            "    ne redémarre jamais l'ordinateur.\n" +
            "/log log.txt\n" +
            "    écrit un journal dans le fichier.\n" +
            "InstallFolder=<dossier> DataFolder=<dossier>\n" +
            "    définit le dossier du programme et le dossier\n" +
            "    des images, paramètres et journaux d'Android.",
            "/install | /repair | /uninstall | /layout [Ordner]\n" +
            "    installiert, repariert, deinstalliert oder kopiert\n" +
            "    die Setupdateien in einen Ordner.\n" +
            "/passive | /quiet\n" +
            "    zeigt nur den Fortschritt oder gar nichts an.\n" +
            "/norestart\n" +
            "    startet den Computer nie neu.\n" +
            "/log log.txt\n" +
            "    schreibt ein Protokoll in die Datei.\n" +
            "InstallFolder=<Ordner> DataFolder=<Ordner>\n" +
            "    legt den Programmordner und den Ordner für die\n" +
            "    Android-Images, Einstellungen und Protokolle fest.",
            "/install | /repair | /uninstall | /layout [pasta]\n" +
            "    instala, repara, desinstala ou copia os arquivos\n" +
            "    de instalação para uma pasta.\n" +
            "/passive | /quiet\n" +
            "    mostra só o progresso, ou nada.\n" +
            "/norestart\n" +
            "    nunca reinicia o computador.\n" +
            "/log log.txt\n" +
            "    grava um log no arquivo.\n" +
            "InstallFolder=<pasta> DataFolder=<pasta>\n" +
            "    define a pasta do programa e a pasta das\n" +
            "    imagens, configurações e logs do Android.",
            "/install | /repair | /uninstall | /layout [папка]\n" +
            "    устанавливает, восстанавливает, удаляет или\n" +
            "    копирует файлы установки в папку.\n" +
            "/passive | /quiet\n" +
            "    показывает только ход работы или ничего.\n" +
            "/norestart\n" +
            "    никогда не перезагружает компьютер.\n" +
            "/log log.txt\n" +
            "    записывает журнал в файл.\n" +
            "InstallFolder=<папка> DataFolder=<папка>\n" +
            "    задаёт папку программы и папку для образов,\n" +
            "    параметров и журналов Android.",
            "/install | /repair | /uninstall | /layout [cartella]\n" +
            "    installa, ripara, disinstalla o copia i file di\n" +
            "    installazione in una cartella.\n" +
            "/passive | /quiet\n" +
            "    mostra solo l'avanzamento, o niente del tutto.\n" +
            "/norestart\n" +
            "    non riavvia mai il computer.\n" +
            "/log log.txt\n" +
            "    scrive un log nel file.\n" +
            "InstallFolder=<cartella> DataFolder=<cartella>\n" +
            "    imposta la cartella del programma e quella per\n" +
            "    le immagini, le impostazioni e i log di Android.",
            "/install | /repair | /uninstall | /layout [thư mục]\n" +
            "    cài đặt, sửa chữa, gỡ cài đặt hoặc sao chép tệp\n" +
            "    cài đặt vào một thư mục.\n" +
            "/passive | /quiet\n" +
            "    chỉ hiện tiến trình, hoặc không hiện gì cả.\n" +
            "/norestart\n" +
            "    không bao giờ khởi động lại máy tính.\n" +
            "/log log.txt\n" +
            "    ghi nhật ký vào tệp.\n" +
            "InstallFolder=<thư mục> DataFolder=<thư mục>\n" +
            "    đặt thư mục chương trình và thư mục chứa ảnh\n" +
            "    hệ thống, cài đặt và nhật ký của Android.",
            "/install | /repair | /uninstall | /layout [folder]\n" +
            "    memasang, memperbaiki, mencopot, atau menyalin\n" +
            "    file penyiapan ke folder.\n" +
            "/passive | /quiet\n" +
            "    hanya menampilkan progres, atau tidak sama sekali.\n" +
            "/norestart\n" +
            "    tidak pernah memulai ulang komputer.\n" +
            "/log log.txt\n" +
            "    menulis log ke file tersebut.\n" +
            "InstallFolder=<folder> DataFolder=<folder>\n" +
            "    menetapkan folder program dan folder untuk\n" +
            "    image, pengaturan, dan log Android.",
            "/install | /repair | /uninstall | /layout [klasör]\n" +
            "    yükler, onarır, kaldırır veya kurulum\n" +
            "    dosyalarını bir klasöre kopyalar.\n" +
            "/passive | /quiet\n" +
            "    yalnızca ilerlemeyi gösterir ya da hiç göstermez.\n" +
            "/norestart\n" +
            "    bilgisayarı asla yeniden başlatmaz.\n" +
            "/log log.txt\n" +
            "    dosyaya bir günlük yazar.\n" +
            "InstallFolder=<klasör> DataFolder=<klasör>\n" +
            "    program klasörünü ve Android görüntüleri,\n" +
            "    ayarları ve günlükleri için klasörü belirler.");

        public static string InstallButton => Pick(
            "Install",
            "설치",
            "インストール",
            "安装",
            "安裝",
            "Instalar",
            "Installer",
            "Installieren",
            "Instalar",
            "Установить",
            "Installa",
            "Cài đặt",
            "Pasang",
            "Yükle");
        public static string CancelButton => Pick(
            "Cancel",
            "취소",
            "キャンセル",
            "取消",
            "取消",
            "Cancelar",
            "Annuler",
            "Abbrechen",
            "Cancelar",
            "Отмена",
            "Annulla",
            "Hủy",
            "Batal",
            "İptal");
        public static string BackButton => Pick(
            "Back",
            "뒤로",
            "戻る",
            "返回",
            "返回",
            "Atrás",
            "Retour",
            "Zurück",
            "Voltar",
            "Назад",
            "Indietro",
            "Quay lại",
            "Kembali",
            "Geri");
        public static string CloseButton => Pick(
            "Close",
            "닫기",
            "閉じる",
            "关闭",
            "關閉",
            "Cerrar",
            "Fermer",
            "Schließen",
            "Fechar",
            "Закрыть",
            "Chiudi",
            "Đóng",
            "Tutup",
            "Kapat");
        public static string DoneButton => Pick(
            "Done",
            "완료",
            "完了",
            "完成",
            "完成",
            "Listo",
            "Terminé",
            "Fertig",
            "Concluir",
            "Готово",
            "Fine",
            "Xong",
            "Selesai",
            "Bitti");
        public static string OkButton => Pick(
            "OK",
            "확인",
            "OK",
            "确定",
            "確定",
            "Aceptar",
            "OK",
            "OK",
            "OK",
            "ОК",
            "OK",
            "OK",
            "OK",
            "Tamam");
        public static string RepairButton => Pick(
            "Repair",
            "복구",
            "修復",
            "修复",
            "修復",
            "Reparar",
            "Réparer",
            "Reparieren",
            "Reparar",
            "Восстановить",
            "Ripara",
            "Sửa chữa",
            "Perbaiki",
            "Onar");
        public static string UninstallButton => Pick(
            "Uninstall",
            "제거",
            "アンインストール",
            "卸载",
            "解除安裝",
            "Desinstalar",
            "Désinstaller",
            "Deinstallieren",
            "Desinstalar",
            "Удалить",
            "Disinstalla",
            "Gỡ cài đặt",
            "Copot pemasangan",
            "Kaldır");
        public static string RestartButton => Pick(
            "Restart",
            "다시 시작",
            "再起動",
            "重启",
            "重新啟動",
            "Reiniciar",
            "Redémarrer",
            "Neu starten",
            "Reiniciar",
            "Перезагрузить",
            "Riavvia",
            "Khởi động lại",
            "Mulai ulang",
            "Yeniden başlat");
        public static string Minimize => Pick(
            "Minimize",
            "최소화",
            "最小化",
            "最小化",
            "最小化",
            "Minimizar",
            "Réduire",
            "Minimieren",
            "Minimizar",
            "Свернуть",
            "Riduci a icona",
            "Thu nhỏ",
            "Minimalkan",
            "Simge durumuna küçült");

        public static string ConfirmCancel => Pick(
            "Stop setup?",
            "설치를 멈출까요?",
            "セットアップを中止しますか?",
            "要停止安装吗？",
            "要停止安裝嗎？",
            "¿Desea detener la instalación?",
            "Arrêter l'installation ?",
            "Setup beenden?",
            "Parar a instalação?",
            "Остановить установку?",
            "Interrompere l'installazione?",
            "Dừng cài đặt?",
            "Hentikan penyiapan?",
            "Kurulum durdurulsun mu?");
    }
}
