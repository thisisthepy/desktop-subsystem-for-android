//! The app's text in the user's language, through gettext.
//!
//! Every string the app shows is written in English in the code and wrapped in [`tr`] or
//! [`trf`]. gettext looks it up in the translation of the user's language (`po/<language>.po`,
//! installed as `/usr/share/locale/<language>/LC_MESSAGES/dsa-settings.mo`) and gives the
//! English back when there is none. Placeholders are `{0}`, `{1}` and so on, filled by
//! [`fill`], so a translation can put them in another order.

/// Where the translations are installed.
pub const LOCALE_DIR: &str = "/usr/share/locale";

/// Sets the locale from the environment and points gettext at the app's translations.
/// `DSA_SETTINGS_LOCALE_DIR` names another folder, for running from a build.
pub fn init() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        let dir = std::env::var("DSA_SETTINGS_LOCALE_DIR").unwrap_or_else(|_| LOCALE_DIR.into());
        gnu::init(crate::TEXT_DOMAIN, &dir);
    }
}

/// `text` in the user's language.
pub fn tr(text: &str) -> String {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        gnu::gettext(text)
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    {
        text.to_string()
    }
}

/// `template` with `{0}` replaced by the first of `values`, `{1}` by the second, and so on.
pub fn fill(template: &str, values: &[&str]) -> String {
    let mut out = template.to_string();
    for (index, value) in values.iter().enumerate() {
        out = out.replace(&format!("{{{index}}}"), value);
    }
    out
}

/// [`tr`] of `template`, then [`fill`].
pub fn trf(template: &str, values: &[&str]) -> String {
    fill(&tr(template), values)
}

/// gettext as the C library has it. glibc carries gettext itself, so nothing more is linked.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod gnu {
    use std::ffi::{CStr, CString};
    use std::os::raw::c_char;

    extern "C" {
        #[link_name = "gettext"]
        fn c_gettext(msgid: *const c_char) -> *mut c_char;
        #[link_name = "textdomain"]
        fn c_textdomain(domain: *const c_char) -> *mut c_char;
        #[link_name = "bindtextdomain"]
        fn c_bindtextdomain(domain: *const c_char, dir: *const c_char) -> *mut c_char;
        #[link_name = "bind_textdomain_codeset"]
        fn c_bind_textdomain_codeset(domain: *const c_char, codeset: *const c_char) -> *mut c_char;
    }

    pub fn init(domain: &str, dir: &str) {
        let (Ok(domain), Ok(dir)) = (CString::new(domain), CString::new(dir)) else {
            return;
        };
        // SAFETY: every argument is a NUL-terminated string that outlives the call; the C
        // library copies what it keeps.
        unsafe {
            libc::setlocale(libc::LC_ALL, c"".as_ptr());
            c_bindtextdomain(domain.as_ptr(), dir.as_ptr());
            c_bind_textdomain_codeset(domain.as_ptr(), c"UTF-8".as_ptr());
            c_textdomain(domain.as_ptr());
        }
    }

    pub fn gettext(text: &str) -> String {
        let Ok(id) = CString::new(text) else {
            return text.to_string();
        };
        // SAFETY: gettext returns either its argument or a string of the loaded catalogue,
        // both NUL-terminated and alive while this copies it.
        unsafe {
            CStr::from_ptr(c_gettext(id.as_ptr()))
                .to_string_lossy()
                .into_owned()
        }
    }
}
