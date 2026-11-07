//! What this computer says it is, for the device identity of each user's Android: the
//! firmware's system and board strings (`/sys/class/dmi/id/`), the processor (`/proc/cpuinfo`)
//! and the computer's name.
//!
//! The computer's name is the one GNOME Settings shows under About, Device Name: the pretty
//! host name in `/etc/machine-info`, else the host name.
//!
//! Read at every start of Android, so a renamed computer is followed from the next start.

use std::path::Path;

use dsa_host_core::computer_identity::ComputerInfo;

/// What this computer says it is now.
pub fn read() -> ComputerInfo {
    read_from(Path::new("/"))
}

/// What the computer whose files are under `root` says it is, for tests on a fake tree.
pub fn read_from(root: &Path) -> ComputerInfo {
    let text = |path: &str| {
        std::fs::read_to_string(root.join(path))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let dmi = |name: &str| text(&format!("sys/class/dmi/id/{name}"));
    let (cpu_vendor, cpu_name) = cpu_of(&text("proc/cpuinfo"));
    let mut product = dmi("product_name");
    if product.is_empty() {
        // A computer described by a device tree (many ARM ones) has no DMI table.
        product = text("sys/firmware/devicetree/base/model")
            .trim_end_matches('\0')
            .to_string();
    }
    let pretty = pretty_hostname(&text("etc/machine-info"));
    ComputerInfo {
        system_manufacturer: dmi("sys_vendor"),
        system_product: product,
        system_family: dmi("product_family"),
        system_version: dmi("product_version"),
        baseboard_product: dmi("board_name"),
        computer_name: if pretty.is_empty() {
            text("proc/sys/kernel/hostname")
        } else {
            pretty
        },
        cpu_vendor,
        cpu_name,
    }
}

/// `PRETTY_HOSTNAME` from the text of `/etc/machine-info`, unquoted.
pub fn pretty_hostname(machine_info: &str) -> String {
    machine_info
        .lines()
        .find_map(|line| line.trim().strip_prefix("PRETTY_HOSTNAME="))
        .map(|value| {
            let value = value.trim();
            let unquoted = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            unquoted.replace("\\\"", "\"").replace("\\\\", "\\")
        })
        .unwrap_or_default()
}

/// The CPU's vendor and brand string from the text of `/proc/cpuinfo`: `vendor_id` and
/// `model name` on x86, the implementer's name and `model name` or `Hardware` on ARM.
pub fn cpu_of(cpuinfo: &str) -> (String, String) {
    let field = |key: &str| {
        cpuinfo.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            (name.trim() == key).then(|| value.trim().to_string())
        })
    };
    let vendor = field("vendor_id").unwrap_or_else(|| {
        let implementer = field("CPU implementer").unwrap_or_default();
        match implementer.to_ascii_lowercase().as_str() {
            "0x41" => "ARM",
            "0x42" => "Broadcom",
            "0x4e" => "NVIDIA",
            "0x51" => "Qualcomm",
            "0x61" => "Apple",
            "0x6d" => "Microsoft",
            "0xc0" => "Ampere",
            _ => "",
        }
        .to_string()
    });
    let name = field("model name")
        .or_else(|| field("Hardware"))
        .unwrap_or_default();
    (vendor, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr17_the_computer_is_read_from_dmi_cpuinfo_and_its_pretty_name() {
        let root = std::env::temp_dir().join(format!("dsa-linux-identity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let write = |path: &str, text: &str| {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        };
        write(
            "sys/class/dmi/id/sys_vendor",
            "SAMSUNG ELECTRONICS CO., LTD.\n",
        );
        write("sys/class/dmi/id/product_name", "960XGL\n");
        write("sys/class/dmi/id/product_family", "Galaxy Book4 Ultra\n");
        write("sys/class/dmi/id/product_version", "P10ALX\n");
        write("sys/class/dmi/id/board_name", "NT960XGP-G72AG\n");
        write(
            "proc/cpuinfo",
            "processor\t: 0\nvendor_id\t: GenuineIntel\nmodel name\t: Intel(R) Core(TM) Ultra 7 155H\n",
        );
        write("proc/sys/kernel/hostname", "brew-book\n");
        write(
            "etc/machine-info",
            "PRETTY_HOSTNAME=\"BREW's Book\"\nCHASSIS=laptop\n",
        );
        let info = read_from(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(info.system_manufacturer, "SAMSUNG ELECTRONICS CO., LTD.");
        assert_eq!(info.baseboard_product, "NT960XGP-G72AG");
        assert_eq!(info.computer_name, "BREW's Book");
        let identity = info.identity();
        assert_eq!(identity.manufacturer.value, "Samsung");
        assert_eq!(identity.model.value, "NT960XGP-G72AG");
        assert_eq!(identity.model_name.value, "Galaxy Book4 Ultra");
        assert_eq!(identity.soc_model.value, "Core Ultra 7 155H");
    }

    #[test]
    fn fr17_an_arm_cpu_is_named_by_its_implementer() {
        let (vendor, name) = cpu_of("processor : 0\nCPU implementer : 0x51\nCPU part : 0x001\n");
        assert_eq!(vendor, "Qualcomm");
        assert_eq!(name, "");
        assert_eq!(pretty_hostname("CHASSIS=laptop\n"), "");
        assert_eq!(pretty_hostname("PRETTY_HOSTNAME=plain\n"), "plain");
    }
}
