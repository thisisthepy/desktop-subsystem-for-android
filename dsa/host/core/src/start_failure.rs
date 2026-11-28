//! Which side a failed start of Android failed on: the host, or the guest.
//!
//! A start the host refused (its hypervisor or network service would not create or start the
//! VM, the VM's process ended before the guest's kernel printed anything, the network could not
//! be attached) says nothing about the Android image: the same image starts once the host is
//! fixed. Such a failure never marks the image failed or rolls it back, and the user is told
//! it is a problem with this computer, with its reason. Only a failure after the guest ran
//! (the boot never finishes, the guest dies while booting) counts against the image.
//!
//! Every platform's launcher says which side its last failed start failed on
//! ([`crate::session::GuestLauncher::failure_side`]); the session and the update service act
//! on it the same way on every host.
//!
//! One host-side failure gets words of its own: a computer whose virtualization is off
//! ([`virtualization_off`]) runs no VM at all until the user turns it on, so every surface
//! names the three causes and how to fix each ([`virtualization_off_text`]) instead of the raw
//! error.

use crate::ui_language::UiLanguage;

/// Where a failed start failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailureSide {
    /// The host did not run the guest: nothing of the image was run.
    Host,
    /// The guest ran and failed: the image may be to blame.
    #[default]
    Guest,
}

impl FailureSide {
    /// The side of a start whose guest `printed` something (its kernel ran) or not.
    pub fn from_guest_output(printed: bool) -> FailureSide {
        if printed {
            FailureSide::Guest
        } else {
            FailureSide::Host
        }
    }

    /// The side of a boot that ended, from its console: `Some(printed)` when the console was
    /// kept, `None` when it could not be kept for this boot. Without a console nothing shows
    /// that the guest's kernel ran, and a VM process that died at start looks the same as a
    /// guest that died later, so the image is not blamed: it stays on trial, and the next
    /// start tries it again.
    pub fn from_console(printed: Option<bool>) -> FailureSide {
        FailureSide::from_guest_output(printed.unwrap_or(false))
    }
}

/// What a host-side failure's reason starts with, in the session's English account of it
/// (the status's last reason). Every surface that shows the status finds a host problem by it.
pub const HOST_PROBLEM_PREFIX: &str =
    "this computer could not run Android (a problem with the host, not with Android's image): ";

/// The English account of a host-side failure for `why`.
pub fn host_problem(why: &str) -> String {
    format!("{HOST_PROBLEM_PREFIX}{why}")
}

/// The reason of a host-side failure, when `reason` (a status's last reason) is one.
pub fn host_problem_reason(reason: &str) -> Option<&str> {
    reason.strip_prefix(HOST_PROBLEM_PREFIX)
}

/// The sentence that tells the user, in `lang`, that this computer could not run Android, with
/// `why` after it as the host reported it.
pub fn host_problem_text(lang: UiLanguage, why: &str) -> String {
    let words = match lang {
        UiLanguage::English => {
            "This computer could not run Android. This is a problem with the computer, not with Android; the Android version in use is kept. Reason: {why}"
        }
        UiLanguage::Korean => {
            "이 컴퓨터에서 Android를 실행하지 못했습니다. Android가 아닌 컴퓨터 쪽 문제이며, 쓰던 Android 버전은 그대로 유지됩니다. 이유: {why}"
        }
        UiLanguage::Japanese => {
            "このコンピューターで Android を実行できませんでした。Android ではなくコンピューター側の問題で、使用中の Android のバージョンはそのまま保たれます。理由: {why}"
        }
        UiLanguage::ChineseSimplified => {
            "此电脑无法运行 Android。这是电脑的问题，不是 Android 的问题，正在使用的 Android 版本会保留。原因：{why}"
        }
        UiLanguage::ChineseTraditional => {
            "這台電腦無法執行 Android。這是電腦的問題，不是 Android 的問題，正在使用的 Android 版本會保留。原因：{why}"
        }
        UiLanguage::Spanish => {
            "Este equipo no pudo ejecutar Android. Es un problema del equipo, no de Android; se conserva la versión de Android en uso. Motivo: {why}"
        }
        UiLanguage::French => {
            "Cet ordinateur n'a pas pu exécuter Android. Le problème vient de l'ordinateur, pas d'Android ; la version d'Android utilisée est conservée. Raison : {why}"
        }
        UiLanguage::German => {
            "Dieser Computer konnte Android nicht ausführen. Das Problem liegt am Computer, nicht an Android; die verwendete Android-Version bleibt erhalten. Grund: {why}"
        }
        UiLanguage::PortugueseBrazil => {
            "Este computador não conseguiu executar o Android. O problema é do computador, não do Android; a versão do Android em uso é mantida. Motivo: {why}"
        }
        UiLanguage::Russian => {
            "Этому компьютеру не удалось запустить Android. Проблема в компьютере, а не в Android; используемая версия Android сохраняется. Причина: {why}"
        }
        UiLanguage::Italian => {
            "Questo computer non è riuscito a eseguire Android. Il problema è del computer, non di Android; la versione di Android in uso viene mantenuta. Motivo: {why}"
        }
        UiLanguage::Vietnamese => {
            "Máy tính này không chạy được Android. Đây là sự cố của máy tính, không phải của Android; phiên bản Android đang dùng được giữ nguyên. Lý do: {why}"
        }
        UiLanguage::Indonesian => {
            "Komputer ini tidak dapat menjalankan Android. Ini masalah komputer, bukan Android; versi Android yang digunakan tetap dipertahankan. Alasan: {why}"
        }
        UiLanguage::Turkish => {
            "Bu bilgisayar Android'i çalıştıramadı. Sorun Android'de değil bilgisayardadır; kullanılan Android sürümü korunur. Neden: {why}"
        }
    };
    words.replace("{why}", why)
}

/// The text a surface shows for a status's last reason, in `lang`: a host problem in the
/// user's language with its reason, and what to turn on when virtualization is off; `None` for
/// any other reason.
pub fn localized_host_problem(lang: UiLanguage, reason: &str) -> Option<String> {
    if virtualization_off(reason) {
        return Some(virtualization_off_text(lang));
    }
    let first = reason.lines().next().unwrap_or_default();
    host_problem_reason(first).map(|why| host_problem_text(lang, why))
}

/// The account a host gives when it found, without trying to start a VM, that its hypervisor
/// is not running. A start failure that says it is read as [`virtualization_off`].
pub const HYPERVISOR_NOT_RUNNING: &str =
    "the hypervisor is not running on this computer, so virtualization is off";

/// Phrases of a start failure that mean the computer runs no hypervisor, in lower case: the
/// hypervisor service's own words for it, and [`HYPERVISOR_NOT_RUNNING`].
const VIRTUALIZATION_OFF_PHRASES: [&str; 3] = [
    "host compute system is not available",
    "hyper-v is not installed",
    HYPERVISOR_NOT_RUNNING,
];

/// Error codes a hypervisor service answers with when no hypervisor runs on the computer: the
/// hypervisor is not installed (`0x80370102`), and none is present (`0xc0351000`). They arrive
/// as text in the start failure's reason.
const VIRTUALIZATION_OFF_CODES: [&str; 2] = ["0x80370102", "0xc0351000"];

/// The error codes (`0x` and eight hex digits) in `line`, in lower case.
fn error_codes(line: &str) -> impl Iterator<Item = String> + '_ {
    line.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| {
            token.len() == 10
                && token.starts_with("0x")
                && token[2..].chars().all(|c| c.is_ascii_hexdigit())
        })
        .map(str::to_ascii_lowercase)
}

/// Whether a start failure's reason (or a status's last reason) says the computer could not
/// run the VM because virtualization is off: the processor's virtualization is turned off in
/// the firmware, the hypervisor feature is not installed, or the hypervisor is kept from
/// starting by the boot configuration. Only the first line counts: the lines below it are the
/// guest's console, whose hex numbers are addresses.
///
/// Such a failure is always the host's: no image is to blame for it.
pub fn virtualization_off(reason: &str) -> bool {
    let first = reason.lines().next().unwrap_or_default();
    let lower = first.to_ascii_lowercase();
    VIRTUALIZATION_OFF_PHRASES
        .iter()
        .any(|phrase| lower.contains(phrase))
        || error_codes(first).any(|code| VIRTUALIZATION_OFF_CODES.contains(&code.as_str()))
}

/// Whether a failed image's recorded reason (the first line of its trial's reason) says the
/// host, not the image, kept it from starting.
pub fn host_side_reason(reason: &str) -> bool {
    let first = reason.lines().next().unwrap_or_default();
    host_problem_reason(first).is_some() || virtualization_off(first)
}

/// The sentence that virtualization is off, and the three things to check, in `lang`.
fn virtualization_words(lang: UiLanguage) -> (&'static str, &'static str) {
    match lang {
        UiLanguage::English => (
            "This computer cannot run Android because virtualization is off",
            "The Android version in use is kept. Check these three, then restart the computer: 1. Turn on virtualization (Intel VT-x or AMD-V) in the computer's UEFI/BIOS settings; when it is on, Task Manager shows \"Virtualization: Enabled\" under Performance, CPU. 2. Turn on the Windows feature \"Virtual Machine Platform\" (Turn Windows features on or off). 3. The hypervisor must not be turned off in the boot configuration: if `bcdedit` shows `hypervisorlaunchtype Off`, run `bcdedit /set hypervisorlaunchtype auto` as an administrator.",
        ),
        UiLanguage::Korean => (
            "가상화가 꺼져 있어 이 컴퓨터에서 Android를 실행할 수 없습니다",
            "쓰던 Android 버전은 그대로 유지됩니다. 다음 세 가지를 확인한 뒤 컴퓨터를 다시 시작하십시오. 1. 컴퓨터의 UEFI/BIOS 설정에서 가상화(Intel VT-x 또는 AMD-V)를 켭니다. 켜져 있으면 작업 관리자의 성능, CPU에 \"가상화: 사용\"이라고 표시됩니다. 2. Windows 기능 \"가상 머신 플랫폼\"을 켭니다(Windows 기능 켜기/끄기). 3. 부팅 구성에서 하이퍼바이저가 꺼져 있으면 안 됩니다. `bcdedit`에 `hypervisorlaunchtype Off`가 보이면 관리자 권한으로 `bcdedit /set hypervisorlaunchtype auto`를 실행합니다.",
        ),
        UiLanguage::Japanese => (
            "仮想化がオフになっているため、このコンピューターで Android を実行できません",
            "使用中の Android のバージョンはそのまま保たれます。次の 3 つを確認してから、コンピューターを再起動してください。1. コンピューターの UEFI/BIOS 設定で仮想化 (Intel VT-x または AMD-V) をオンにします。オンのときは、タスク マネージャーの [パフォーマンス] の CPU に「仮想化: 有効」と表示されます。2. Windows の機能「仮想マシン プラットフォーム」をオンにします ([Windows の機能の有効化または無効化])。3. ブート構成でハイパーバイザーがオフになっていてはいけません。`bcdedit` に `hypervisorlaunchtype Off` と表示される場合は、管理者として `bcdedit /set hypervisorlaunchtype auto` を実行します。",
        ),
        UiLanguage::ChineseSimplified => (
            "此电脑的虚拟化已关闭，因此无法运行 Android",
            "正在使用的 Android 版本会保留。请检查以下三项，然后重新启动电脑：1. 在电脑的 UEFI/BIOS 设置中开启虚拟化（Intel VT-x 或 AMD-V）。开启后，任务管理器的“性能”>“CPU”中会显示“虚拟化: 已启用”。2. 开启 Windows 功能“虚拟机平台”（启用或关闭 Windows 功能）。3. 启动配置中不能关闭虚拟机监控程序：如果 `bcdedit` 显示 `hypervisorlaunchtype Off`，请以管理员身份运行 `bcdedit /set hypervisorlaunchtype auto`。",
        ),
        UiLanguage::ChineseTraditional => (
            "這台電腦的虛擬化已關閉，因此無法執行 Android",
            "正在使用的 Android 版本會保留。請檢查以下三項，然後重新啟動電腦：1. 在電腦的 UEFI/BIOS 設定中開啟虛擬化（Intel VT-x 或 AMD-V）。開啟後，工作管理員的「效能」>「CPU」中會顯示「虛擬化: 已啟用」。2. 開啟 Windows 功能「虛擬機器平台」（開啟或關閉 Windows 功能）。3. 開機設定中不能關閉 Hypervisor：如果 `bcdedit` 顯示 `hypervisorlaunchtype Off`，請以系統管理員身分執行 `bcdedit /set hypervisorlaunchtype auto`。",
        ),
        UiLanguage::Spanish => (
            "Este equipo no puede ejecutar Android porque la virtualización está desactivada",
            "Se conserva la versión de Android en uso. Comprueba estos tres puntos y reinicia el equipo: 1. Activa la virtualización (Intel VT-x o AMD-V) en la configuración UEFI/BIOS del equipo; cuando está activada, el Administrador de tareas muestra \"Virtualización: Habilitado\" en Rendimiento, CPU. 2. Activa la característica de Windows \"Plataforma de máquina virtual\" (Activar o desactivar las características de Windows). 3. El hipervisor no debe estar desactivado en la configuración de arranque: si `bcdedit` muestra `hypervisorlaunchtype Off`, ejecuta `bcdedit /set hypervisorlaunchtype auto` como administrador.",
        ),
        UiLanguage::French => (
            "Cet ordinateur ne peut pas exécuter Android, car la virtualisation est désactivée",
            "La version d'Android utilisée est conservée. Vérifiez ces trois points, puis redémarrez l'ordinateur : 1. Activez la virtualisation (Intel VT-x ou AMD-V) dans les paramètres UEFI/BIOS de l'ordinateur ; lorsqu'elle est activée, le Gestionnaire des tâches affiche « Virtualisation : Activé » dans Performance, Processeur. 2. Activez la fonctionnalité Windows « Plateforme de machine virtuelle » (Activer ou désactiver des fonctionnalités Windows). 3. L'hyperviseur ne doit pas être désactivé dans la configuration de démarrage : si `bcdedit` affiche `hypervisorlaunchtype Off`, exécutez `bcdedit /set hypervisorlaunchtype auto` en tant qu'administrateur.",
        ),
        UiLanguage::German => (
            "Dieser Computer kann Android nicht ausführen, weil die Virtualisierung ausgeschaltet ist",
            "Die verwendete Android-Version bleibt erhalten. Prüfen Sie diese drei Punkte und starten Sie den Computer dann neu: 1. Schalten Sie die Virtualisierung (Intel VT-x oder AMD-V) in den UEFI/BIOS-Einstellungen des Computers ein; ist sie an, zeigt der Task-Manager unter Leistung, CPU „Virtualisierung: Aktiviert“. 2. Schalten Sie das Windows-Feature „VM-Plattform“ ein (Windows-Features aktivieren oder deaktivieren). 3. Der Hypervisor darf in der Startkonfiguration nicht ausgeschaltet sein: Zeigt `bcdedit` `hypervisorlaunchtype Off`, führen Sie `bcdedit /set hypervisorlaunchtype auto` als Administrator aus.",
        ),
        UiLanguage::PortugueseBrazil => (
            "Este computador não pode executar o Android porque a virtualização está desativada",
            "A versão do Android em uso é mantida. Verifique estes três itens e reinicie o computador: 1. Ative a virtualização (Intel VT-x ou AMD-V) nas configurações UEFI/BIOS do computador; quando ativada, o Gerenciador de Tarefas mostra \"Virtualização: Habilitado\" em Desempenho, CPU. 2. Ative o recurso do Windows \"Plataforma de Máquina Virtual\" (Ativar ou desativar recursos do Windows). 3. O hipervisor não pode estar desativado na configuração de inicialização: se `bcdedit` mostrar `hypervisorlaunchtype Off`, execute `bcdedit /set hypervisorlaunchtype auto` como administrador.",
        ),
        UiLanguage::Russian => (
            "Этот компьютер не может запустить Android, потому что виртуализация выключена",
            "Используемая версия Android сохраняется. Проверьте три пункта и перезагрузите компьютер: 1. Включите виртуализацию (Intel VT-x или AMD-V) в настройках UEFI/BIOS компьютера; когда она включена, диспетчер задач показывает «Виртуализация: Включено» в разделе «Производительность», «ЦП». 2. Включите компонент Windows «Платформа виртуальной машины» (Включение или отключение компонентов Windows). 3. Гипервизор не должен быть отключён в конфигурации загрузки: если `bcdedit` показывает `hypervisorlaunchtype Off`, выполните `bcdedit /set hypervisorlaunchtype auto` от имени администратора.",
        ),
        UiLanguage::Italian => (
            "Questo computer non può eseguire Android perché la virtualizzazione è disattivata",
            "La versione di Android in uso viene mantenuta. Controlla questi tre punti, poi riavvia il computer: 1. Attiva la virtualizzazione (Intel VT-x o AMD-V) nelle impostazioni UEFI/BIOS del computer; quando è attiva, Gestione attività mostra \"Virtualizzazione: Abilitato\" in Prestazioni, CPU. 2. Attiva la funzionalità di Windows \"Piattaforma macchina virtuale\" (Attiva o disattiva funzionalità Windows). 3. L'hypervisor non deve essere disattivato nella configurazione di avvio: se `bcdedit` mostra `hypervisorlaunchtype Off`, esegui `bcdedit /set hypervisorlaunchtype auto` come amministratore.",
        ),
        UiLanguage::Vietnamese => (
            "Máy tính này không chạy được Android vì ảo hóa đang tắt",
            "Phiên bản Android đang dùng được giữ nguyên. Hãy kiểm tra ba điều sau rồi khởi động lại máy tính: 1. Bật ảo hóa (Intel VT-x hoặc AMD-V) trong cài đặt UEFI/BIOS của máy tính; khi đã bật, Trình quản lý Tác vụ hiển thị \"Ảo hóa: Đã bật\" trong Hiệu suất, CPU. 2. Bật tính năng Windows \"Nền tảng Máy ảo\" (Bật hoặc tắt tính năng Windows). 3. Hypervisor không được tắt trong cấu hình khởi động: nếu `bcdedit` hiển thị `hypervisorlaunchtype Off`, hãy chạy `bcdedit /set hypervisorlaunchtype auto` với quyền quản trị.",
        ),
        UiLanguage::Indonesian => (
            "Komputer ini tidak dapat menjalankan Android karena virtualisasi mati",
            "Versi Android yang digunakan tetap dipertahankan. Periksa tiga hal ini, lalu mulai ulang komputer: 1. Aktifkan virtualisasi (Intel VT-x atau AMD-V) di pengaturan UEFI/BIOS komputer; jika sudah aktif, Pengelola Tugas menampilkan \"Virtualisasi: Diaktifkan\" di Kinerja, CPU. 2. Aktifkan fitur Windows \"Platform Mesin Virtual\" (Aktifkan atau nonaktifkan fitur Windows). 3. Hypervisor tidak boleh dimatikan di konfigurasi boot: jika `bcdedit` menampilkan `hypervisorlaunchtype Off`, jalankan `bcdedit /set hypervisorlaunchtype auto` sebagai administrator.",
        ),
        UiLanguage::Turkish => (
            "Sanallaştırma kapalı olduğu için bu bilgisayar Android'i çalıştıramıyor",
            "Kullanılan Android sürümü korunur. Şu üç noktayı denetleyin, ardından bilgisayarı yeniden başlatın: 1. Bilgisayarın UEFI/BIOS ayarlarında sanallaştırmayı (Intel VT-x veya AMD-V) açın; açıkken Görev Yöneticisi Performans, CPU altında \"Sanallaştırma: Etkin\" gösterir. 2. \"Sanal Makine Platformu\" Windows özelliğini açın (Windows özelliklerini aç veya kapat). 3. Hipervizör önyükleme yapılandırmasında kapatılmamış olmalıdır: `bcdedit` `hypervisorlaunchtype Off` gösteriyorsa `bcdedit /set hypervisorlaunchtype auto` komutunu yönetici olarak çalıştırın.",
        ),
    }
}

/// The title of the notice that virtualization is off, in `lang`.
pub fn virtualization_off_title(lang: UiLanguage) -> &'static str {
    virtualization_words(lang).0
}

/// What a surface says, in `lang`, when the computer cannot run Android because
/// virtualization is off: that sentence, then the three causes and how to fix each.
pub fn virtualization_off_text(lang: UiLanguage) -> String {
    let (title, steps) = virtualization_words(lang);
    format!("{title}. {steps}")
}

/// The key of the host's notice that virtualization is off. It is under the prefix of the
/// host's notices about recoveries, so every platform shows it as DSA's own.
pub const VIRTUALIZATION_NOTICE_KEY: &str = "dsa:recovery:virtualization-off";

/// The host's notice, in `lang`, that the computer cannot run Android because virtualization
/// is off, with what to turn on.
pub fn virtualization_notification(lang: UiLanguage) -> crate::notifications::Notification {
    let (title, steps) = virtualization_words(lang);
    crate::notifications::Notification {
        key: VIRTUALIZATION_NOTICE_KEY.into(),
        package_name: String::new(),
        app_label: crate::update::alerts::SHOWN_AS.into(),
        title: title.into(),
        text: steps.into(),
        icon_png: Vec::new(),
        importance: 4,
        high_priority: false,
        clearable: true,
        when_millis: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_millis() as i64)
            .unwrap_or(0),
        buttons: Vec::new(),
    }
}

/// Follows the status a window manager reads and says when to tell the user that
/// virtualization is off: at the first look when it is already so, since the user has to act
/// on it before Android can run at all, and again at each new stop for it.
#[derive(Debug, Default)]
pub struct VirtualizationNotices {
    /// The state last seen, and its reason.
    last: Option<(crate::session::SessionState, Option<String>)>,
}

impl VirtualizationNotices {
    pub fn new() -> VirtualizationNotices {
        VirtualizationNotices::default()
    }

    /// Reads the status `state` with `last_reason`. Returns whether to tell the user now.
    pub fn observe(
        &mut self,
        state: crate::session::SessionState,
        last_reason: Option<&str>,
    ) -> bool {
        let now = (state, last_reason.map(str::to_string));
        let before = self.last.replace(now.clone());
        state == crate::session::SessionState::Stopped
            && last_reason.is_some_and(virtualization_off)
            && before.as_ref() != Some(&now)
    }
}
