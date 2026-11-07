//! What the computer Android runs on says it is, and the device identity that follows from it.
//!
//! Each host reads the computer's firmware tables (SMBIOS) and its operating system each time
//! Android starts, and hands the raw strings here as a [`ComputerInfo`]. The rules here turn
//! them into the values Android reports by default: the manufacturer and brand, the model, the
//! model name, the device name and the chip. Firmware strings are written by each maker in its
//! own way, and many leave the fields they do not use with a placeholder such as
//! `To be filled by O.E.M.`; a value that is missing or a placeholder falls back, field by
//! field, to DSA's own.
//!
//! Nothing here names a host operating system API. The SMBIOS reader works on the bytes of
//! the tables, which every host can get in its own way.

use crate::device_identity::{DEFAULT_BRAND, DEFAULT_MANUFACTURER, DEFAULT_MODEL};

/// The longest manufacturer, model, model name or chip name passed on.
const MAX_TEXT: usize = 64;
/// The longest brand passed on, as a brand is a short name.
const MAX_BRAND: usize = 32;
/// The longest device name passed on, in characters.
pub const MAX_DEVICE_NAME: usize = 64;

/// The raw strings a host reads from the computer's firmware and operating system. An empty
/// string is a value the host could not read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComputerInfo {
    /// The system manufacturer (SMBIOS type 1), such as `SAMSUNG ELECTRONICS CO., LTD.`.
    pub system_manufacturer: String,
    /// The system product name (SMBIOS type 1), such as `960XGL`.
    pub system_product: String,
    /// The system family (SMBIOS type 1), such as `Galaxy Book4 Ultra`.
    pub system_family: String,
    /// The system version (SMBIOS type 1), such as `P10ALX`.
    pub system_version: String,
    /// The baseboard product name (SMBIOS type 2), such as `NT960XGP-G72AG`.
    pub baseboard_product: String,
    /// The name the user gave the computer, as the operating system's settings show it.
    pub computer_name: String,
    /// The CPU's vendor as the processor or the operating system names it, such as
    /// `GenuineIntel`, `AuthenticAMD`, `Apple` or `Qualcomm Technologies Inc`.
    pub cpu_vendor: String,
    /// The CPU's brand string, such as `Intel(R) Core(TM) Ultra 7 155H`.
    pub cpu_name: String,
}

impl ComputerInfo {
    /// Fills each value this one lacks, or holds a placeholder for, from `other`.
    pub fn fill_from(&mut self, other: &ComputerInfo) {
        let fill = |mine: &mut String, theirs: &String| {
            if usable(mine).is_none() && usable(theirs).is_some() {
                *mine = theirs.clone();
            }
        };
        fill(&mut self.system_manufacturer, &other.system_manufacturer);
        fill(&mut self.system_product, &other.system_product);
        fill(&mut self.system_family, &other.system_family);
        fill(&mut self.system_version, &other.system_version);
        fill(&mut self.baseboard_product, &other.baseboard_product);
        fill(&mut self.computer_name, &other.computer_name);
        fill(&mut self.cpu_vendor, &other.cpu_vendor);
        fill(&mut self.cpu_name, &other.cpu_name);
    }

    /// The device identity these strings give, each value with where it came from.
    pub fn identity(&self) -> ComputerIdentity {
        let manufacturer =
            tidy_manufacturer(&self.system_manufacturer).filter(|m| printable(m, MAX_TEXT));
        let brand = manufacturer.as_deref().and_then(brand_of);
        let model = model_of(self);
        let model_name = model_name_of(self);
        let device_name = device_name_of(&self.computer_name);
        let soc_manufacturer = soc_manufacturer_of(&self.cpu_vendor, &self.cpu_name);
        let soc_model = soc_model_of(&self.cpu_name);
        ComputerIdentity {
            manufacturer: Derived::or(manufacturer, DEFAULT_MANUFACTURER),
            brand: Derived::or(brand, DEFAULT_BRAND),
            model: Derived::or(model, DEFAULT_MODEL),
            model_name: Derived::or(model_name, DEFAULT_MODEL),
            device_name: Derived::or(device_name, ""),
            soc_manufacturer: Derived::or(soc_manufacturer, ""),
            soc_model: Derived::or(soc_model, ""),
        }
    }
}

/// Where a default value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The computer's firmware or operating system.
    Computer,
    /// DSA's own value, because the computer gave none that could be used.
    Dsa,
}

impl Origin {
    /// The name the local API and `dsa settings device --json` give it.
    pub fn key(self) -> &'static str {
        match self {
            Origin::Computer => "computer",
            Origin::Dsa => "dsa",
        }
    }
}

/// A default value and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derived {
    pub value: String,
    pub origin: Origin,
}

impl Derived {
    fn or(value: Option<String>, dsa: &str) -> Derived {
        match value {
            Some(value) => Derived {
                value,
                origin: Origin::Computer,
            },
            None => Derived {
                value: dsa.to_string(),
                origin: Origin::Dsa,
            },
        }
    }

    /// DSA's own value.
    pub fn dsa(value: &str) -> Derived {
        Derived::or(None, value)
    }
}

/// The defaults the computer gives. An empty device name or chip value is one the computer
/// did not give: Android then keeps its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerIdentity {
    pub manufacturer: Derived,
    pub brand: Derived,
    pub model: Derived,
    /// The name the product is sold under, such as `Galaxy Book4 Ultra`.
    pub model_name: Derived,
    /// The computer's name, which becomes Android's device name.
    pub device_name: Derived,
    pub soc_manufacturer: Derived,
    pub soc_model: Derived,
}

impl ComputerIdentity {
    /// DSA's own values, for a computer that said nothing.
    pub fn dsa() -> ComputerIdentity {
        ComputerInfo::default().identity()
    }
}

impl Default for ComputerIdentity {
    fn default() -> Self {
        ComputerIdentity::dsa()
    }
}

/// The strings firmware makers leave in the fields they do not fill, lower-cased.
const PLACEHOLDERS: &[&str] = &[
    "to be filled by o.e.m.",
    "to be filled by oem",
    "to be filled by o.e.m",
    "default string",
    "default",
    "system product name",
    "system manufacturer",
    "system manufacturer name",
    "system version",
    "system family",
    "system sku",
    "system name",
    "base board product name",
    "baseboard product name",
    "base board",
    "type1productconfigid",
    "type2 - board product name1",
    "o.e.m.",
    "o.e.m",
    "oem",
    "none",
    "null",
    "not specified",
    "not applicable",
    "not available",
    "n/a",
    "na",
    "unknown",
    "invalid",
    "undefined",
    "empty",
    "123456789",
    "0123456789",
    "x.x",
];

/// Whether `value` is a placeholder rather than a value: empty, one of the strings firmware
/// makers leave, or with no letter or digit at all.
pub fn is_placeholder(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() || !value.chars().any(|c| c.is_alphanumeric()) {
        return true;
    }
    let lower = value.to_lowercase();
    if PLACEHOLDERS.contains(&lower.as_str()) {
        return true;
    }
    // The same character over and over, such as 00000000 or XXXXXX.
    let mut chars = lower.chars();
    let first = chars.next();
    value.len() > 1 && chars.all(|c| Some(c) == first)
}

/// `value` trimmed, with runs of spaces made one, unless it is a placeholder.
fn usable(value: &str) -> Option<String> {
    if is_placeholder(value) {
        return None;
    }
    Some(value.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Whether `value` is 1 to `max` printable ASCII characters, as Android's manufacturer and
/// model may be.
fn printable(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && value.chars().all(|c| (' '..='~').contains(&c))
}

/// The makers whose firmware name is not the name they sell under: what it starts with,
/// lower-cased, and that name.
const MAKERS: &[(&str, &str)] = &[
    ("samsung", "Samsung"),
    ("dell", "Dell"),
    ("lenovo", "Lenovo"),
    ("hewlett-packard", "HP"),
    ("hewlett packard", "HP"),
    ("hp", "HP"),
    ("asustek", "ASUS"),
    ("asus", "ASUS"),
    ("micro-star", "MSI"),
    ("micro star", "MSI"),
    ("msi", "MSI"),
    ("acer", "Acer"),
    ("microsoft", "Microsoft"),
    ("apple", "Apple"),
    ("gigabyte", "Gigabyte"),
    ("lg electronics", "LG"),
    ("huawei", "Huawei"),
    ("honor", "Honor"),
    ("xiaomi", "Xiaomi"),
    ("razer", "Razer"),
    ("framework", "Framework"),
    ("toshiba", "Toshiba"),
    ("dynabook", "Dynabook"),
    ("fujitsu", "Fujitsu"),
    ("panasonic", "Panasonic"),
    ("sony", "Sony"),
    ("google", "Google"),
    ("system76", "System76"),
    ("vmware", "VMware"),
];

/// The legal forms that end a company's name, lower-cased, longest first.
const LEGAL_SUFFIXES: &[&str] = &[
    "co., ltd.",
    "co.,ltd.",
    "co., ltd",
    "co.,ltd",
    "co. ltd.",
    "co ltd",
    "corporation",
    "incorporated",
    "limited",
    "l.l.c.",
    "corp.",
    "corp",
    "inc.",
    "inc",
    "ltd.",
    "ltd",
    "llc",
    "gmbh",
    "s.a.",
    "b.v.",
    "plc",
    "k.k.",
    "co.",
    "ag",
];

/// The system manufacturer as people know it: the maker's own name for the makers that write
/// a longer one, else the name without its legal form, with words written in capitals
/// spelled in title case (`LENOVO` is `Lenovo`; a short one such as `HP` stays).
pub fn tidy_manufacturer(raw: &str) -> Option<String> {
    let raw = usable(raw)?;
    let lower = raw.to_lowercase();
    for &(start, name) in MAKERS {
        // A prefix that is a whole word, or the whole name.
        if let Some(rest) = lower.strip_prefix(start) {
            if !rest.starts_with(|c: char| c.is_ascii_alphanumeric()) {
                return Some(name.to_string());
            }
        }
    }
    let mut name = raw.clone();
    loop {
        let trimmed = name.trim_end_matches([' ', ',']).to_string();
        if !trimmed.is_ascii() {
            // A name in another script keeps its own form.
            name = trimmed;
            break;
        }
        let lower = trimmed.to_ascii_lowercase();
        let suffix = LEGAL_SUFFIXES.iter().find(|suffix| {
            lower.ends_with(*suffix)
                && lower.len() > suffix.len()
                && lower[..lower.len() - suffix.len()].ends_with([' ', ','])
        });
        match suffix {
            Some(suffix) => name = trimmed[..trimmed.len() - suffix.len()].to_string(),
            None => {
                name = trimmed;
                break;
            }
        }
    }
    let words: Vec<String> = name
        .split_whitespace()
        .map(|word| {
            let letters: Vec<char> = word.chars().filter(|c| c.is_alphabetic()).collect();
            if letters.len() > 3 && letters.iter().all(|c| c.is_uppercase()) {
                let mut chars = word.chars();
                let first = chars.next().map(|c| c.to_string()).unwrap_or_default();
                first + &chars.as_str().to_lowercase()
            } else {
                word.to_string()
            }
        })
        .collect();
    let name = words.join(" ");
    (!name.is_empty()).then_some(name)
}

/// Whether `value` may be a brand: 1 to 32 letters, digits, `.`, `_` and `-`, starting with
/// a letter or a digit.
fn brand_like(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_BRAND
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && value.starts_with(|c: char| c.is_ascii_alphanumeric())
}

/// The brand a manufacturer gives: the manufacturer itself when it may be a brand, else its
/// first word, as a brand has no spaces.
pub fn brand_of(manufacturer: &str) -> Option<String> {
    if brand_like(manufacturer) {
        return Some(manufacturer.to_string());
    }
    let first = manufacturer.split_whitespace().next()?;
    brand_like(first).then(|| first.to_string())
}

/// The first run of two or more digits in `value` and the letter right after it, upper-cased:
/// `960X` in both `960XGL` and `NT960XGP-G72AG`.
fn stem(value: &str) -> Option<(String, Option<char>)> {
    let chars: Vec<char> = value.chars().collect();
    let start = chars.iter().position(|c| c.is_ascii_digit())?;
    let end = chars[start..]
        .iter()
        .position(|c| !c.is_ascii_digit())
        .map_or(chars.len(), |n| start + n);
    if end - start < 2 {
        return None;
    }
    let digits: String = chars[start..end].iter().collect();
    let letter = chars
        .get(end)
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_uppercase());
    Some((digits, letter))
}

/// Whether two product names are of one family: the same, or with the same model number
/// stem (`960XGL` and `NT960XGP-G72AG`).
pub fn same_family(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    matches!((stem(a), stem(b)), (Some(x), Some(y)) if x == y && x.1.is_some())
}

/// The model Android reports: the baseboard product when it is of the system product's family,
/// because some makers put the sale model number there, else the system product.
fn model_of(info: &ComputerInfo) -> Option<String> {
    let product = usable(&info.system_product).filter(|p| printable(p, MAX_TEXT));
    let board = usable(&info.baseboard_product).filter(|b| printable(b, MAX_TEXT));
    match (product, board) {
        (Some(product), Some(board)) if same_family(&product, &board) => Some(board),
        (Some(product), _) => Some(product),
        (None, _) => None,
    }
}

/// The name the product is sold under: the system family, else the system version when it
/// reads as a name (it holds a letter), else the system product.
fn model_name_of(info: &ComputerInfo) -> Option<String> {
    let ok = |value: &String| printable(value, MAX_TEXT);
    usable(&info.system_family)
        .filter(ok)
        .or_else(|| {
            usable(&info.system_version)
                .filter(|v| v.chars().any(|c| c.is_alphabetic()))
                .filter(ok)
        })
        .or_else(|| usable(&info.system_product).filter(ok))
}

/// The computer's name as Android's device name: trimmed, without control characters, at
/// most [`MAX_DEVICE_NAME`] characters.
pub fn device_name_of(raw: &str) -> Option<String> {
    let cleaned: String = raw.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return None;
    }
    let name: String = cleaned.chars().take(MAX_DEVICE_NAME).collect();
    Some(name.trim_end().to_string())
}

/// The chip's maker, from the CPU vendor, or from its brand string when the vendor says
/// nothing known.
pub fn soc_manufacturer_of(vendor: &str, cpu_name: &str) -> Option<String> {
    let known = |text: &str| -> Option<&'static str> {
        let lower = text.to_lowercase();
        if lower.contains("genuineintel") || lower.contains("intel") {
            Some("Intel")
        } else if lower.contains("authenticamd") || lower.contains("amd") || lower.contains("ryzen")
        {
            Some("AMD")
        } else if lower.contains("qualcomm") || lower.contains("snapdragon") {
            Some("Qualcomm")
        } else if lower.contains("apple") {
            Some("Apple")
        } else if lower.contains("mediatek") {
            Some("MediaTek")
        } else if lower.contains("nvidia") {
            Some("NVIDIA")
        } else {
            None
        }
    };
    if let Some(name) = known(vendor).or_else(|| known(cpu_name)) {
        return Some(name.to_string());
    }
    usable(vendor)
        .and_then(|v| tidy_manufacturer(&v))
        .filter(|v| printable(v, MAX_TEXT))
}

/// The chip's model from the CPU's brand string, without the maker, the trademark signs, the
/// generation, the clock speed and the graphics: `Intel(R) Core(TM) Ultra 7 155H` is
/// `Core Ultra 7 155H`, `AMD Ryzen 7 7840U w/ Radeon 780M Graphics` is `Ryzen 7 7840U`,
/// `Apple M3` is `M3`, and `Snapdragon(R) X Elite - X1E78100 - Qualcomm(R) Oryon(TM) CPU`
/// is `Snapdragon X Elite`.
pub fn soc_model_of(cpu_name: &str) -> Option<String> {
    let mut name = usable(cpu_name)?;
    for sign in ["(R)", "(r)", "(TM)", "(tm)", "(C)", "\u{ae}", "\u{2122}"] {
        name = name.replace(sign, "");
    }
    for cut in [" @ ", " - ", " w/ ", " with ", " With "] {
        if let Some(at) = name.find(cut) {
            name.truncate(at);
        }
    }
    let mut words: Vec<&str> = name.split_whitespace().collect();
    // "13th Gen Intel Core i7-1360P": the generation goes, the model number says it.
    if words.len() > 2 && words[1].eq_ignore_ascii_case("gen") {
        let ordinal = words[0].to_lowercase();
        if ordinal.ends_with("st")
            || ordinal.ends_with("nd")
            || ordinal.ends_with("rd")
            || ordinal.ends_with("th")
        {
            words.drain(..2);
        }
    }
    if let Some(first) = words.first() {
        if words.len() > 1
            && ["intel", "amd", "apple", "qualcomm", "mediatek", "nvidia"]
                .contains(&first.to_lowercase().as_str())
        {
            words.remove(0);
        }
    }
    // "8-Core Processor", "Processor", "CPU" at the end.
    while let Some(last) = words.last() {
        let lower = last.to_lowercase();
        if lower == "cpu" || lower == "processor" || lower.ends_with("-core") {
            words.pop();
        } else {
            break;
        }
    }
    let model = words.join(" ");
    (printable(&model, MAX_TEXT) && !is_placeholder(&model)).then_some(model)
}

/// The values of SMBIOS structure tables: the system (type 1), the baseboard (type 2) and the
/// processor (type 4). `tables` is the table area, starting with the first structure; a
/// structure cut short ends the reading with what was read so far.
pub fn from_smbios(tables: &[u8]) -> ComputerInfo {
    let mut info = ComputerInfo::default();
    let mut at = 0usize;
    let mut seen_processor = false;
    while at + 4 <= tables.len() {
        let kind = tables[at];
        let length = tables[at + 1] as usize;
        if length < 4 || at + length > tables.len() {
            break;
        }
        let formatted = &tables[at..at + length];
        // The strings follow the formatted area, each ending with a zero, and the set ends
        // with a second zero.
        let mut strings: Vec<String> = Vec::new();
        let mut end = at + length;
        loop {
            let Some(len) = tables[end..].iter().position(|&b| b == 0) else {
                return info;
            };
            if len == 0 {
                end += 1;
                if strings.is_empty() {
                    // A structure with no strings ends with two zeros.
                    end += usize::from(tables.get(end) == Some(&0));
                }
                break;
            }
            strings.push(String::from_utf8_lossy(&tables[end..end + len]).into_owned());
            end += len + 1;
        }
        let text = |offset: usize| -> String {
            formatted
                .get(offset)
                .and_then(|&n| (n as usize).checked_sub(1))
                .and_then(|n| strings.get(n))
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };
        match kind {
            1 => {
                info.system_manufacturer = text(0x04);
                info.system_product = text(0x05);
                info.system_version = text(0x06);
                info.system_family = text(0x1a);
            }
            2 if info.baseboard_product.is_empty() => {
                info.baseboard_product = text(0x05);
            }
            4 if !seen_processor => {
                seen_processor = true;
                info.cpu_vendor = text(0x07);
                info.cpu_name = text(0x10);
            }
            127 => break,
            _ => {}
        }
        at = end;
    }
    info
}
