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
/// user's language with its reason, `None` for any other reason.
pub fn localized_host_problem(lang: UiLanguage, reason: &str) -> Option<String> {
    let first = reason.lines().next().unwrap_or_default();
    host_problem_reason(first).map(|why| host_problem_text(lang, why))
}
