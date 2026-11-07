//! The names, version information and icons every DSA program carries, as resources.
//!
//! The desktop names a program by its version information (`FileDescription`) and shows the
//! first icon group in it. Task Manager and the taskbar take both from the executable, so
//! every DSA program carries them: the service, the window manager, the `dsa` command and
//! the app window host. Each app's own window host carries the app's name and icon instead
//! (see `dsa_host_windows::app_exe`).
//!
//! The bytes are built here in plain Rust, so the build scripts need no resource compiler:
//! [`embed`] writes a `.res` file the linker takes as it is, and the window manager uses
//! [`version_info`] and [`icon_group`] to rewrite a copy of the app window host.
//!
//! [`names`] holds every name the desktop shows, in one place.

pub mod names {
    //! What the desktop calls each DSA program. Renaming one is a change here alone.

    /// The product every DSA program belongs to.
    pub const PRODUCT: &str = "Desktop Subsystem for Android";
    /// Who publishes it.
    pub const COMPANY: &str = "ThisIsThePy";
    /// The service (`dsa-hostd.exe`), as Task Manager lists it.
    pub const SERVICE: &str = "Desktop Subsystem for Android Service";
    /// The window manager (`dsa-wm.exe`), as Task Manager lists it.
    pub const WINDOW_MANAGER: &str = "DSA Window Manager";
    /// The window manager's name on a Korean desktop.
    pub const WINDOW_MANAGER_KO: &str = "Android용 창 관리자";
    /// The command line tool (`dsa.exe`).
    pub const CLI: &str = "Desktop Subsystem for Android command line";
    /// The app window host as installed (`dsa-app-window.exe`). Each app's copy carries the
    /// app's own name instead.
    pub const APP_WINDOW: &str = "DSA app window";
}

pub mod shell {
    //! The words Explorer, the Start menu and the Services console show for DSA, in every
    //! language they are translated into. They are a string table in the installed
    //! `dsa-app-window.exe`, and the installer points at them as indirect strings
    //! (`@<file>,-<id>`), so Windows shows each user the words in that user's display
    //! language. Product names (DSA Files, DSA Settings) are not translated; the verbs are.

    /// "Open in an Android app", the context menu entry of every file.
    pub const OPEN_IN_ANDROID_APP: u16 = 101;
    /// "Share with an Android app", the context menu entry of every file.
    pub const SHARE_WITH_ANDROID_APP: u16 = 102;
    /// The description of the "DSA Settings" Start menu shortcut.
    pub const SETTINGS_SHORTCUT_DESCRIPTION: u16 = 104;
    /// "Install in Android", the open verb of an `.apk` file.
    pub const INSTALL_IN_ANDROID: u16 = 105;
    /// The kind of file an `.apk` file is, as Explorer's Type column shows it.
    pub const APK_FILE_TYPE: u16 = 106;
    /// The name of the entry in Explorer's navigation pane. Android is a trademark and is
    /// never transliterated, so it reads the same in every language.
    pub const NAVIGATION_NAME: u16 = 107;
    /// The tooltip of the entry in Explorer's navigation pane.
    pub const NAVIGATION_TIP: u16 = 108;
    /// The description of the DSA service, as the Services console shows it.
    pub const SERVICE_DESCRIPTION: u16 = 109;
    /// The APK Installer's name, as Open with and the taskbar show it.
    pub const APK_INSTALLER_NAME: u16 = 110;
    /// The kind of file a split-APK bundle (`.xapk`, `.apks`, `.apkm`) is, as Explorer's Type
    /// column shows it.
    pub const APK_BUNDLE_FILE_TYPE: u16 = 111;

    /// Every string, as (ID, English, Korean).
    pub const STRINGS: &[(u16, &str, &str)] = &[
        (OPEN_IN_ANDROID_APP, "Open in an Android app", "Android 앱에서 열기"),
        (SHARE_WITH_ANDROID_APP, "Share with an Android app", "Android 앱으로 공유"),
        (
            SETTINGS_SHORTCUT_DESCRIPTION,
            "Settings and state of Desktop Subsystem for Android",
            "Desktop Subsystem for Android의 설정과 상태",
        ),
        (INSTALL_IN_ANDROID, "Install in Android", "Android에 설치"),
        (APK_FILE_TYPE, "APK app package", "APK 앱 패키지"),
        (NAVIGATION_NAME, "Android", "Android"),
        (
            NAVIGATION_TIP,
            "The shared storage of your Android",
            "내 Android의 공유 저장소",
        ),
        (
            SERVICE_DESCRIPTION,
            "Runs each user's Android in its own virtual machine and answers the dsa command and the desktop apps.",
            "사용자마다 Android를 자체 가상 머신에서 실행하고, dsa 명령과 데스크톱 앱의 요청에 응답합니다.",
        ),
        (APK_INSTALLER_NAME, "APK Installer", "APK 설치 관리자"),
        (APK_BUNDLE_FILE_TYPE, "Split APK bundle", "분할 APK 묶음"),
    ];

    /// The English string `id`, which the installer also writes as the plain fallback.
    pub fn english(id: u16) -> Option<&'static str> {
        STRINGS.iter().find(|s| s.0 == id).map(|s| s.1)
    }

    /// The Korean string `id`.
    pub fn korean(id: u16) -> Option<&'static str> {
        STRINGS.iter().find(|s| s.0 == id).map(|s| s.2)
    }
}

/// English (United States), the language of every program's main version information.
pub const LANG_EN_US: u16 = 0x0409;
/// Korean, for the names that have a Korean translation.
pub const LANG_KO_KR: u16 = 0x0412;
/// The code page the strings are in: Unicode.
pub const CODE_PAGE_UNICODE: u16 = 1200;

/// `RT_ICON`: one image of an icon.
pub const RT_ICON: u16 = 3;
/// `RT_GROUP_ICON`: the list of an icon's images.
pub const RT_GROUP_ICON: u16 = 14;
/// `RT_STRING`: a block of sixteen strings of a string table.
pub const RT_STRING: u16 = 6;
/// `RT_VERSION`: the version information.
pub const RT_VERSION: u16 = 16;
/// `RT_RCDATA`: raw data the program reads itself.
pub const RT_RCDATA: u16 = 10;

/// The `RT_RCDATA` ID of the app's icon as the guest drew it, a PNG larger than the 256 pixel
/// icon image, in an app's window program. The window's splash is drawn from it, so the icon
/// is scaled down to its size on a monitor of any scale up to 320% rather than up from 256.
pub const SPLASH_ICON_ID: u16 = 1;

/// The largest image an icon resource holds, in pixels on a side.
pub const MAX_ICON_IMAGE_SIDE: u32 = 256;

/// The ID of the icon group every DSA program shows, the first and only one.
pub const ICON_GROUP_ID: u16 = 1;
/// The ID of the version information.
pub const VERSION_ID: u16 = 1;
/// The ID of the second icon group the installed window host carries: the icon of Android's
/// entry in Explorer's navigation pane (DSA Files), which the entry names as
/// `dsa-app-window.exe,-3`. It was 2 while the icon carried a Files badge; Explorer caches
/// an icon by file and index, so a new index makes an upgrade show the new drawing.
pub const FILES_ICON_GROUP_ID: u16 = 3;
/// The `RT_ICON` ID of the Files icon's first image, clear of the program icon's images.
pub const FILES_ICON_FIRST_IMAGE_ID: u16 = 101;

/// The strings of one language's version information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionStrings {
    /// The name Task Manager and the taskbar show.
    pub description: String,
    pub product: String,
    pub company: String,
    /// The file's own name without `.exe`.
    pub internal_name: String,
    /// The release, such as `0.1.8`.
    pub version: String,
}

/// A release number such as `0.1.8` as the four 16-bit parts version information holds.
/// Parts that are missing or not numbers are zero.
pub fn version_parts(version: &str) -> [u16; 4] {
    let mut parts = [0u16; 4];
    let numbers = version
        .split(['.', '-', '+'])
        .map(|part| part.parse::<u16>().unwrap_or(0));
    for (slot, number) in parts.iter_mut().zip(numbers) {
        *slot = number;
    }
    parts
}

fn utf16z(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn pad4(buffer: &mut Vec<u8>) {
    while buffer.len() % 4 != 0 {
        buffer.push(0);
    }
}

enum Value<'a> {
    None,
    Text(&'a str),
    Binary(&'a [u8]),
}

/// One block of version information: its length, value length, type and key, its value,
/// then its children, each on a four-byte boundary.
fn block(key: &str, value: Value<'_>, children: &[Vec<u8>]) -> Vec<u8> {
    let (value_bytes, value_length, kind) = match value {
        Value::None => (Vec::new(), 0u16, 1u16),
        Value::Text(text) => {
            let bytes = utf16z(text);
            let units = (bytes.len() / 2) as u16;
            (bytes, units, 1)
        }
        Value::Binary(bytes) => (bytes.to_vec(), bytes.len() as u16, 0),
    };
    let mut out = vec![0, 0];
    out.extend_from_slice(&value_length.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&utf16z(key));
    pad4(&mut out);
    out.extend_from_slice(&value_bytes);
    for child in children {
        pad4(&mut out);
        out.extend_from_slice(child);
    }
    let length = out.len() as u16;
    out[0..2].copy_from_slice(&length.to_le_bytes());
    out
}

/// The `VS_VERSIONINFO` resource for an application in `language`, whose strings are
/// `strings`.
pub fn version_info(strings: &VersionStrings, language: u16) -> Vec<u8> {
    let [a, b, c, d] = version_parts(&strings.version);
    let ms = (u32::from(a) << 16) | u32::from(b);
    let ls = (u32::from(c) << 16) | u32::from(d);
    let fixed: Vec<u8> = [
        0xFEEF_04BDu32, // signature
        0x0001_0000,    // structure version
        ms,
        ls,
        ms,
        ls,
        0x3F,        // the flags that are valid
        0,           // no flags
        0x0004_0004, // VOS_NT_WINDOWS32
        1,           // VFT_APP
        0,
        0,
        0,
    ]
    .iter()
    .flat_map(|word| word.to_le_bytes())
    .collect();
    let file_name = format!("{}.exe", strings.internal_name);
    let entries = [
        ("CompanyName", strings.company.as_str()),
        ("FileDescription", strings.description.as_str()),
        ("FileVersion", strings.version.as_str()),
        ("InternalName", strings.internal_name.as_str()),
        ("OriginalFilename", file_name.as_str()),
        ("ProductName", strings.product.as_str()),
        ("ProductVersion", strings.version.as_str()),
    ];
    let strings_children: Vec<Vec<u8>> = entries
        .iter()
        .map(|(key, value)| block(key, Value::Text(value), &[]))
        .collect();
    let table_key = format!("{language:04X}{CODE_PAGE_UNICODE:04X}");
    let table = block(&table_key, Value::None, &strings_children);
    let string_file_info = block("StringFileInfo", Value::None, &[table]);
    let translation = [language.to_le_bytes(), CODE_PAGE_UNICODE.to_le_bytes()].concat();
    let var = block("Translation", Value::Binary(&translation), &[]);
    let var_file_info = block("VarFileInfo", Value::None, &[var]);
    block(
        "VS_VERSION_INFO",
        Value::Binary(&fixed),
        &[string_file_info, var_file_info],
    )
}

/// One image of an icon: its size in pixels (0 for 256 or more), its bits per pixel, and its
/// bytes, a PNG image or a bitmap without its file header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconImage {
    pub width: u8,
    pub height: u8,
    pub bit_count: u16,
    pub data: Vec<u8>,
}

/// The images of an `.ico` file, in the order the file lists them.
pub fn parse_ico(file: &[u8]) -> Result<Vec<IconImage>, String> {
    let word = |at: usize| -> Result<u16, String> {
        file.get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .ok_or_else(|| "the icon file ends early".to_string())
    };
    let dword = |at: usize| -> Result<u32, String> {
        file.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or_else(|| "the icon file ends early".to_string())
    };
    if word(0)? != 0 || word(2)? != 1 {
        return Err("not an icon file".into());
    }
    let count = usize::from(word(4)?);
    (0..count)
        .map(|index| {
            let entry = 6 + 16 * index;
            let size = dword(entry + 8)? as usize;
            let offset = dword(entry + 12)? as usize;
            let data = file
                .get(offset..offset + size)
                .ok_or("an icon image lies outside the file")?
                .to_vec();
            Ok(IconImage {
                width: file[entry],
                height: file[entry + 1],
                bit_count: word(entry + 6)?,
                data,
            })
        })
        .collect()
}

/// A PNG image as an icon image, sized from its header. `None` when it is not a PNG image.
pub fn png_icon(png: &[u8]) -> Option<IconImage> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if png.len() < 24 || png[..8] != SIGNATURE || &png[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let height = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    if width == 0 || height == 0 {
        return None;
    }
    let side = |pixels: u32| if pixels >= 256 { 0 } else { pixels as u8 };
    Some(IconImage {
        width: side(width),
        height: side(height),
        bit_count: 32,
        data: png.to_vec(),
    })
}

/// The side an icon image made from an app's `side` pixel PNG takes: the PNG's own when it
/// fits an icon image, else the largest an icon image holds, to which it is scaled down.
pub fn icon_image_side(side: u32) -> u32 {
    side.min(MAX_ICON_IMAGE_SIDE)
}

/// An icon image of `side` pixels square from `pixels`, rows of straight-alpha BGRA pixels
/// with the top row first: a bitmap without its file header, as `RT_ICON` holds one. The
/// header gives twice the height, for the colour rows and the mask under them; the colour
/// rows go bottom first, and the mask is all clear because the alpha channel says what shows.
/// `None` when `pixels` does not hold `side` rows of `side` pixels.
pub fn dib_icon(pixels: &[u8], side: u32) -> Option<IconImage> {
    let side_usize = side as usize;
    if side == 0 || side > MAX_ICON_IMAGE_SIDE || pixels.len() != side_usize * side_usize * 4 {
        return None;
    }
    let row = side_usize * 4;
    // Mask rows are padded to four bytes.
    let mask_row = side_usize.div_ceil(32) * 4;
    let mut data = Vec::with_capacity(40 + row * side_usize + mask_row * side_usize);
    data.extend_from_slice(&40u32.to_le_bytes());
    data.extend_from_slice(&(side as i32).to_le_bytes());
    data.extend_from_slice(&(2 * side as i32).to_le_bytes());
    data.extend_from_slice(&1u16.to_le_bytes()); // planes
    data.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
    data.extend_from_slice(&0u32.to_le_bytes()); // no compression
    data.extend_from_slice(&((row + mask_row) as u32 * side).to_le_bytes());
    data.extend_from_slice(&[0u8; 16]); // resolution and colours: unused
    for y in (0..side_usize).rev() {
        data.extend_from_slice(&pixels[y * row..(y + 1) * row]);
    }
    data.resize(data.len() + mask_row * side_usize, 0);
    let byte = |side: u32| if side >= 256 { 0 } else { side as u8 };
    Some(IconImage {
        width: byte(side),
        height: byte(side),
        bit_count: 32,
        data,
    })
}

/// The `RT_GROUP_ICON` resource listing `images`, whose `RT_ICON` resources have the IDs
/// `first_id`, `first_id + 1` and so on.
pub fn icon_group(images: &[IconImage], first_id: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(6 + 14 * images.len());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    for (index, image) in images.iter().enumerate() {
        out.push(image.width);
        out.push(image.height);
        out.push(0); // colours: none for true colour
        out.push(0);
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&image.bit_count.to_le_bytes());
        out.extend_from_slice(&(image.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(first_id + index as u16).to_le_bytes());
    }
    out
}

/// The `RT_STRING` resources of a string table in `language`, from (ID, text) pairs. Windows
/// keeps strings in blocks of sixteen: block `n` (from 1) holds IDs `16 * (n - 1)` to
/// `16 * n - 1`, each as its length in UTF-16 units and then the units, with no terminator.
pub fn string_table(strings: &[(u16, &str)], language: u16) -> Vec<Resource> {
    let mut blocks: std::collections::BTreeMap<u16, [Option<&str>; 16]> = Default::default();
    for (id, text) in strings {
        blocks.entry(id / 16 + 1).or_insert([None; 16])[usize::from(id % 16)] = Some(text);
    }
    blocks
        .into_iter()
        .map(|(block, entries)| {
            let mut data = Vec::new();
            for entry in entries {
                let units: Vec<u16> = entry.unwrap_or("").encode_utf16().collect();
                data.extend_from_slice(&(units.len() as u16).to_le_bytes());
                data.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
            }
            Resource {
                kind: RT_STRING,
                id: block,
                language,
                data,
            }
        })
        .collect()
}

/// The resources of a further icon from the `.ico` file `file`: its images, numbered from
/// `first_id`, and the icon group `group_id` that lists them.
pub fn icon_resources(file: &[u8], group_id: u16, first_id: u16) -> Result<Vec<Resource>, String> {
    let images = parse_ico(file)?;
    if images.is_empty() {
        return Err("the icon file holds no image".into());
    }
    let mut resources: Vec<Resource> = images
        .iter()
        .enumerate()
        .map(|(index, image)| Resource {
            kind: RT_ICON,
            id: first_id + index as u16,
            language: 0,
            data: image.data.clone(),
        })
        .collect();
    resources.push(Resource {
        kind: RT_GROUP_ICON,
        id: group_id,
        language: 0,
        data: icon_group(&images, first_id),
    });
    Ok(resources)
}

/// The English and Korean string tables of the words in [`shell`].
pub fn shell_string_tables() -> Vec<Resource> {
    let english: Vec<(u16, &str)> = shell::STRINGS.iter().map(|s| (s.0, s.1)).collect();
    let korean: Vec<(u16, &str)> = shell::STRINGS.iter().map(|s| (s.0, s.2)).collect();
    let mut tables = string_table(&english, LANG_EN_US);
    tables.extend(string_table(&korean, LANG_KO_KR));
    tables
}

/// One resource for a `.res` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    pub kind: u16,
    pub id: u16,
    pub language: u16,
    pub data: Vec<u8>,
}

/// A `.res` file holding `resources`, which the linker takes as an input.
pub fn res_file(resources: &[Resource]) -> Vec<u8> {
    let mut out = Vec::new();
    // Every .res file starts with an empty entry.
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&32u32.to_le_bytes());
    out.extend_from_slice(&[0xFF, 0xFF, 0, 0, 0xFF, 0xFF, 0, 0]);
    out.extend_from_slice(&[0; 16]);
    for resource in resources {
        out.extend_from_slice(&(resource.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&32u32.to_le_bytes());
        out.extend_from_slice(&0xFFFFu16.to_le_bytes());
        out.extend_from_slice(&resource.kind.to_le_bytes());
        out.extend_from_slice(&0xFFFFu16.to_le_bytes());
        out.extend_from_slice(&resource.id.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // data version
        let flags: u16 = if resource.kind == RT_ICON || resource.kind == RT_GROUP_ICON {
            0x1010 // moveable, discardable
        } else {
            0x0030 // moveable, pure
        };
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&resource.language.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // version
        out.extend_from_slice(&0u32.to_le_bytes()); // characteristics
        out.extend_from_slice(&resource.data);
        pad4(&mut out);
    }
    out
}

/// The resources of a DSA program: its version information in English, and in Korean when
/// `description_ko` is given, and its icon.
pub fn program_resources(
    strings: &VersionStrings,
    description_ko: Option<&str>,
    icon: &[IconImage],
) -> Vec<Resource> {
    let mut resources = vec![Resource {
        kind: RT_VERSION,
        id: VERSION_ID,
        language: LANG_EN_US,
        data: version_info(strings, LANG_EN_US),
    }];
    if let Some(description) = description_ko {
        let korean = VersionStrings {
            description: description.to_string(),
            ..strings.clone()
        };
        resources.push(Resource {
            kind: RT_VERSION,
            id: VERSION_ID,
            language: LANG_KO_KR,
            data: version_info(&korean, LANG_KO_KR),
        });
    }
    for (index, image) in icon.iter().enumerate() {
        resources.push(Resource {
            kind: RT_ICON,
            id: 1 + index as u16,
            language: 0,
            data: image.data.clone(),
        });
    }
    if !icon.is_empty() {
        resources.push(Resource {
            kind: RT_GROUP_ICON,
            id: ICON_GROUP_ID,
            language: 0,
            data: icon_group(icon, 1),
        });
    }
    resources
}

/// For a build script: gives the program `bin` its version information and icon. Writes a
/// `.res` file into `OUT_DIR` and has the linker take it, when the target links with the
/// Microsoft linker; elsewhere it does nothing.
///
/// The version is `DSA_VERSION` when the installer build sets it, else the crate's.
pub fn embed(bin: &str, description: &str, description_ko: Option<&str>, icon_file: &str) {
    embed_with(bin, description, description_ko, icon_file, Vec::new());
}

/// [`embed`], with `extra` resources (such as [`shell_string_tables`]) beside them.
pub fn embed_with(
    bin: &str,
    description: &str,
    description_ko: Option<&str>,
    icon_file: &str,
    extra: Vec<Resource>,
) {
    println!("cargo:rerun-if-env-changed=DSA_VERSION");
    println!("cargo:rerun-if-changed={icon_file}");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if !windows || !msvc {
        return;
    }
    let version = std::env::var("DSA_VERSION")
        .or_else(|_| std::env::var("CARGO_PKG_VERSION"))
        .unwrap_or_default();
    let icon = std::fs::read(icon_file)
        .map_err(|e| e.to_string())
        .and_then(|file| parse_ico(&file))
        .unwrap_or_else(|why| panic!("reading the icon {icon_file}: {why}"));
    let strings = VersionStrings {
        description: description.to_string(),
        product: names::PRODUCT.to_string(),
        company: names::COMPANY.to_string(),
        internal_name: bin.to_string(),
        version,
    };
    let out_dir = std::env::var("OUT_DIR").expect("a build script has OUT_DIR");
    let path = std::path::Path::new(&out_dir).join(format!("{bin}.res"));
    let mut resources = program_resources(&strings, description_ko, &icon);
    resources.extend(extra);
    std::fs::write(&path, res_file(&resources))
        .unwrap_or_else(|why| panic!("writing {}: {why}", path.display()));
    println!("cargo:rustc-link-arg-bin={bin}={}", path.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings() -> VersionStrings {
        VersionStrings {
            description: names::WINDOW_MANAGER.into(),
            product: names::PRODUCT.into(),
            company: names::COMPANY.into(),
            internal_name: "dsa-wm".into(),
            version: "0.1.8".into(),
        }
    }

    fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn fr9_shell_words_have_a_korean_string_table() {
        let tables = shell_string_tables();
        let korean: Vec<&Resource> = tables
            .iter()
            .filter(|r| r.kind == RT_STRING && r.language == LANG_KO_KR)
            .collect();
        assert!(!korean.is_empty());
        for (id, english, ko) in shell::STRINGS {
            assert!(!ko.contains("안드로이드"), "{id} transliterates Android");
            let block = korean.iter().find(|r| r.id == id / 16 + 1).unwrap();
            // Walk to the entry the way LoadString does.
            let mut at = 0;
            for _ in 0..id % 16 {
                let len = u16::from_le_bytes([block.data[at], block.data[at + 1]]) as usize;
                at += 2 + 2 * len;
            }
            let len = u16::from_le_bytes([block.data[at], block.data[at + 1]]) as usize;
            assert_eq!(&block.data[at + 2..at + 2 + 2 * len], utf16(ko).as_slice());
            if *id != shell::NAVIGATION_NAME {
                assert_ne!(english, ko, "{id} has no Korean");
            }
        }
        assert_eq!(
            shell::korean(shell::OPEN_IN_ANDROID_APP),
            Some("Android 앱에서 열기")
        );
        assert_eq!(
            shell::english(shell::SHARE_WITH_ANDROID_APP),
            Some("Share with an Android app")
        );
    }

    #[test]
    fn fr3_a_release_number_becomes_four_parts() {
        assert_eq!(version_parts("0.1.8"), [0, 1, 8, 0]);
        assert_eq!(version_parts("1.2.3.4"), [1, 2, 3, 4]);
        assert_eq!(version_parts("2.0.0-rc1"), [2, 0, 0, 0]);
        assert_eq!(version_parts(""), [0, 0, 0, 0]);
    }

    #[test]
    fn fr3_version_information_names_the_program_in_its_language() {
        let info = version_info(&strings(), LANG_EN_US);
        assert_eq!(
            u16::from_le_bytes([info[0], info[1]]) as usize,
            info.len(),
            "the outer block's length is the whole resource"
        );
        assert_eq!(&info[6..6 + 30], utf16("VS_VERSION_INFO").as_slice());
        assert!(contains(&info, &utf16("DSA Window Manager")));
        assert!(contains(&info, &utf16("040904B0")));
        assert!(contains(&info, &utf16("ThisIsThePy")));
        assert!(contains(&info, &0xFEEF_04BDu32.to_le_bytes()));
        let korean = version_info(
            &VersionStrings {
                description: names::WINDOW_MANAGER_KO.into(),
                ..strings()
            },
            LANG_KO_KR,
        );
        assert!(contains(&korean, &utf16("Android용 창 관리자")));
        assert!(contains(&korean, &utf16("041204B0")));
        assert!(contains(&korean, &[0x12, 0x04, 0xB0, 0x04]));
    }

    #[test]
    fn fr3_a_png_icon_is_sized_from_its_header() {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&256u32.to_be_bytes());
        png.extend_from_slice(&48u32.to_be_bytes());
        let image = png_icon(&png).unwrap();
        assert_eq!((image.width, image.height), (0, 48));
        assert!(png_icon(b"not a png at all, not at all").is_none());
        let group = icon_group(&[image], 1);
        assert_eq!(group.len(), 6 + 14);
        assert_eq!(&group[..6], &[0, 0, 1, 0, 1, 0]);
        assert_eq!(&group[18..20], &1u16.to_le_bytes());
    }

    #[test]
    fn fr3_an_icon_larger_than_an_icon_image_is_scaled_down_to_256() {
        assert_eq!(icon_image_side(512), 256);
        assert_eq!(icon_image_side(256), 256);
        assert_eq!(icon_image_side(192), 192);
    }

    #[test]
    fn fr3_a_bitmap_icon_image_holds_its_rows_bottom_first_over_a_clear_mask() {
        // Two by two: the top row red and green, the bottom blue and white.
        let pixels = [
            [0, 0, 0xFF, 0xFF],
            [0, 0xFF, 0, 0xFF],
            [0xFF, 0, 0, 0x80],
            [0xFF, 0xFF, 0xFF, 0xFF],
        ]
        .concat();
        let image = dib_icon(&pixels, 2).unwrap();
        assert_eq!((image.width, image.height, image.bit_count), (2, 2, 32));
        let data = &image.data;
        assert_eq!(u32::from_le_bytes(data[0..4].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(data[4..8].try_into().unwrap()), 2);
        // Twice the height: the colour rows, then the mask.
        assert_eq!(i32::from_le_bytes(data[8..12].try_into().unwrap()), 4);
        assert_eq!(u16::from_le_bytes(data[14..16].try_into().unwrap()), 32);
        assert_eq!(&data[40..48], &pixels[8..16]);
        assert_eq!(&data[48..56], &pixels[0..8]);
        // One four-byte mask row per row, all clear.
        assert_eq!(data.len(), 40 + 16 + 8);
        assert!(data[56..].iter().all(|b| *b == 0));
        // A 256 pixel image is the largest, and its size is written as 0.
        let big = dib_icon(&vec![0; 256 * 256 * 4], 256).unwrap();
        assert_eq!((big.width, big.height), (0, 0));
        assert_eq!(big.data.len(), 40 + 256 * 256 * 4 + 32 * 256);
        assert!(dib_icon(&vec![0; 512 * 512 * 4], 512).is_none());
        assert!(dib_icon(&pixels[..12], 2).is_none());
    }

    #[test]
    fn fr3_the_programs_icon_file_reads_back() {
        let file = include_bytes!("../../installer/setup-ui/assets/logo.ico");
        let images = parse_ico(file).unwrap();
        assert!(images.len() >= 4);
        assert!(images.iter().any(|image| image.width == 32));
        assert!(parse_ico(b"nonsense").is_err());
    }

    #[test]
    fn fr29_the_files_icon_is_a_second_icon_group_with_every_size() {
        let file = include_bytes!("../icons/files.ico");
        let resources =
            icon_resources(file, FILES_ICON_GROUP_ID, FILES_ICON_FIRST_IMAGE_ID).unwrap();
        let images: Vec<_> = resources.iter().filter(|r| r.kind == RT_ICON).collect();
        assert_eq!(images.len(), 8);
        assert_eq!(images[0].id, FILES_ICON_FIRST_IMAGE_ID);
        let group = resources.iter().find(|r| r.kind == RT_GROUP_ICON).unwrap();
        assert_eq!(group.id, FILES_ICON_GROUP_ID);
        assert_eq!(&group.data[4..6], &8u16.to_le_bytes());
        assert!(icon_resources(b"nonsense", 2, 101).is_err());
    }

    #[test]
    fn fr29_the_files_icon_is_the_plain_symbol_without_a_badge() {
        // The navigation pane entry shows the DSA symbol alone, the same drawing as the
        // program icon, with no folder badge in its corner.
        let files = include_bytes!("../icons/files.ico");
        let program = include_bytes!("../../installer/setup-ui/assets/logo.ico");
        assert!(
            files[..] == program[..],
            "files.ico is not the plain DSA symbol"
        );
        assert_ne!(
            FILES_ICON_GROUP_ID, 2,
            "index 2 is the badged icon Explorer may have cached"
        );
        assert_ne!(FILES_ICON_GROUP_ID, ICON_GROUP_ID);
    }

    #[test]
    fn fr3_a_res_file_holds_each_resource_on_a_four_byte_boundary() {
        let resources = program_resources(
            &strings(),
            Some(names::WINDOW_MANAGER_KO),
            &parse_ico(include_bytes!("../../installer/setup-ui/assets/logo.ico")).unwrap(),
        );
        assert!(resources
            .iter()
            .any(|r| r.kind == RT_VERSION && r.language == LANG_KO_KR));
        let file = res_file(&resources);
        assert_eq!(&file[..8], &[0, 0, 0, 0, 32, 0, 0, 0]);
        // Walk the entries the way the linker does.
        let mut at = 32;
        let mut seen = 0;
        while at < file.len() {
            let size = u32::from_le_bytes(file[at..at + 4].try_into().unwrap()) as usize;
            let header = u32::from_le_bytes(file[at + 4..at + 8].try_into().unwrap()) as usize;
            at += header + size;
            at = at.div_ceil(4) * 4;
            seen += 1;
        }
        assert_eq!(at, file.len());
        assert_eq!(seen, resources.len());
    }
}
