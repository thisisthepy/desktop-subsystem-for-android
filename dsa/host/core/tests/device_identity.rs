//! The device identity a user chooses, and how it reaches the guest at boot.

use std::sync::Arc;

use dsa_host_core::bootconfig;
use dsa_host_core::computer_identity::{
    from_smbios, is_placeholder, same_family, soc_model_of, tidy_manufacturer, ComputerIdentity,
    ComputerInfo, Origin,
};
use dsa_host_core::device_identity::{
    append_overlay, ramdisk_overlay, split_fingerprint, BootIdentity, DeviceIdentity,
    DeviceIdentityStore, Field, FileDeviceIdentities, IdentityReport, ImageBuild,
    MemoryDeviceIdentities, RAMDISK_PROP_PATH,
};

fn build() -> ImageBuild {
    ImageBuild {
        release: "16".into(),
        id: "BP2A.250605.031".into(),
        incremental: "13690935.dsa0123456789ab".into(),
        tags: "release-keys".into(),
        arch: "x86_64".into(),
    }
}

/// A computer that says nothing: every default is DSA's own.
fn dsa() -> ComputerIdentity {
    ComputerIdentity::dsa()
}

/// The firmware of the Galaxy Book4 Ultra this was written on: the sale model number is the
/// baseboard product, and the system SKU (BOOK4U-G72AG) is not read.
fn galaxy_book() -> ComputerInfo {
    ComputerInfo {
        system_manufacturer: "SAMSUNG ELECTRONICS CO., LTD.".into(),
        system_product: "960XGL".into(),
        system_family: "Galaxy Book4 Ultra".into(),
        system_version: "P10ALX".into(),
        baseboard_product: "NT960XGP-G72AG".into(),
        computer_name: "BREW-BOOK".into(),
        cpu_vendor: "GenuineIntel".into(),
        cpu_name: "Intel(R) Core(TM) Ultra 7 155H".into(),
    }
}

/// A Dell whose model is only in the system product; its baseboard product is a part number.
fn dell() -> ComputerInfo {
    ComputerInfo {
        system_manufacturer: "Dell Inc.".into(),
        system_product: "XPS 13 9340".into(),
        system_family: "XPS".into(),
        system_version: "".into(),
        baseboard_product: "0KCWT7".into(),
        computer_name: "DESKTOP-4F2K9QL".into(),
        cpu_vendor: "GenuineIntel".into(),
        cpu_name: "Intel(R) Core(TM) Ultra 7 155H".into(),
    }
}

/// A Lenovo whose family is a placeholder and whose version is the name it is sold under.
fn lenovo() -> ComputerInfo {
    ComputerInfo {
        system_manufacturer: "LENOVO".into(),
        system_product: "21HMCTO1WW".into(),
        system_family: "To be filled by O.E.M.".into(),
        system_version: "ThinkPad X1 Carbon Gen 11".into(),
        baseboard_product: "21HMCTO1WW".into(),
        computer_name: "x1".into(),
        cpu_vendor: "AuthenticAMD".into(),
        cpu_name: "AMD Ryzen 7 7840U w/ Radeon 780M Graphics".into(),
    }
}

/// A board whose maker left every field with a placeholder.
fn placeholders() -> ComputerInfo {
    ComputerInfo {
        system_manufacturer: "To be filled by O.E.M.".into(),
        system_product: "System Product Name".into(),
        system_family: "Default string".into(),
        system_version: "O.E.M.".into(),
        baseboard_product: "None".into(),
        computer_name: "  ".into(),
        cpu_vendor: "".into(),
        cpu_name: "Default string".into(),
    }
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("dsa-device-identity-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// The files in a newc cpio archive, by name, the way the kernel unpacks it.
fn cpio_files(base: &[u8]) -> Vec<(String, u32, Vec<u8>)> {
    let mut files = Vec::new();
    let mut at = 0usize;
    loop {
        let header = &base[at..at + 110];
        assert_eq!(&header[..6], b"070701", "a newc header at {at}");
        let field = |i: usize| {
            u32::from_str_radix(
                std::str::from_utf8(&header[6 + i * 8..14 + i * 8]).unwrap(),
                16,
            )
            .unwrap()
        };
        let mode = field(1);
        let size = field(6) as usize;
        let name_size = field(11) as usize;
        let name = std::str::from_utf8(&base[at + 110..at + 110 + name_size - 1])
            .unwrap()
            .to_string();
        at = (at + 110 + name_size).div_ceil(4) * 4;
        let content = base[at..at + size].to_vec();
        at = (at + size).div_ceil(4) * 4;
        if name == "TRAILER!!!" {
            break;
        }
        files.push((name, mode, content));
    }
    assert_eq!(at, base.len(), "nothing after the trailer");
    files
}

fn property(text: &str, key: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")).map(str::to_string))
}

#[test]
fn fr17_the_defaults_are_dsas_own() {
    let e = DeviceIdentity::default().effective(Some(&build()), &dsa());
    assert_eq!(e.manufacturer, "thisisthepy");
    assert_eq!(e.brand, "DSA");
    assert_eq!(e.model, "Desktop Subsystem for Android");
    assert_eq!(e.device, "dsa_x86_64");
    assert_eq!(e.name, "dsa_x86_64");
    assert_eq!(e.characteristics, "tablet");
    assert_eq!(
        e.fingerprint,
        "DSA/dsa_x86_64/dsa_x86_64:16/BP2A.250605.031/13690935.dsa0123456789ab:user/release-keys"
    );
}

#[test]
fn fr17_every_boot_carries_the_identity_with_the_computers_values() {
    let props =
        DeviceIdentity::default().boot_properties(Some(&build()), &galaxy_book().identity());
    assert_eq!(
        property(&props, "ro.product.manufacturer").as_deref(),
        Some("Samsung")
    );
    assert_eq!(
        property(&props, "ro.product.brand").as_deref(),
        Some("Samsung")
    );
    assert_eq!(
        property(&props, "ro.product.vendor.model").as_deref(),
        Some("NT960XGP-G72AG")
    );
    assert_eq!(
        property(&props, "ro.product.device").as_deref(),
        Some("dsa_x86_64")
    );
    assert_eq!(
        property(&props, "ro.product.name").as_deref(),
        Some("dsa_x86_64")
    );
    assert_eq!(
        property(&props, "ro.product.marketname").as_deref(),
        Some("Galaxy Book4 Ultra")
    );
    assert_eq!(
        property(&props, "ro.soc.manufacturer").as_deref(),
        Some("Intel")
    );
    assert_eq!(
        property(&props, "ro.soc.model").as_deref(),
        Some("Core Ultra 7 155H")
    );
    assert_eq!(
        property(&props, "ro.build.fingerprint").as_deref(),
        Some("Samsung/dsa_x86_64/dsa_x86_64:16/BP2A.250605.031/13690935.dsa0123456789ab:user/release-keys")
    );
    // A computer that says nothing still gets the file, with DSA's values and no chip.
    let props = DeviceIdentity::default().boot_properties(Some(&build()), &dsa());
    assert_eq!(
        property(&props, "ro.product.manufacturer").as_deref(),
        Some("thisisthepy")
    );
    assert_eq!(
        property(&props, "ro.product.model").as_deref(),
        Some("Desktop Subsystem for Android")
    );
    assert_eq!(property(&props, "ro.soc.manufacturer"), None);
    assert_eq!(property(&props, "ro.soc.model"), None);
}

#[test]
fn fr17_an_arm64_image_is_dsa_arm64() {
    let arm = ImageBuild {
        arch: "arm64".into(),
        ..build()
    };
    let e = DeviceIdentity::default().effective(Some(&arm), &dsa());
    assert_eq!(e.device, "dsa_arm64");
    assert_eq!(e.name, "dsa_arm64");
    assert!(e.fingerprint.starts_with("DSA/dsa_arm64/dsa_arm64:16/"));
    let manifest = serde_json::json!({
        "arch": "arm64",
        "identity": {"release": "16", "id": "B", "incremental": "1.dsaab", "tags": "release-keys"}
    });
    assert_eq!(
        ImageBuild::from_manifest(&manifest)
            .unwrap()
            .default_device(),
        "dsa_arm64"
    );
}

#[test]
fn fr17_this_pcs_firmware_gives_its_sale_model_and_name() {
    let c = galaxy_book().identity();
    assert_eq!(c.manufacturer.value, "Samsung");
    assert_eq!(c.brand.value, "Samsung");
    assert_eq!(c.model.value, "NT960XGP-G72AG");
    assert_eq!(c.model_name.value, "Galaxy Book4 Ultra");
    assert_eq!(c.device_name.value, "BREW-BOOK");
    assert_eq!(c.soc_manufacturer.value, "Intel");
    assert_eq!(c.soc_model.value, "Core Ultra 7 155H");
    for derived in [
        &c.manufacturer,
        &c.brand,
        &c.model,
        &c.model_name,
        &c.device_name,
    ] {
        assert_eq!(derived.origin, Origin::Computer);
    }
}

#[test]
fn fr17_a_model_only_in_the_system_product_is_taken_from_there() {
    let c = dell().identity();
    assert_eq!(c.manufacturer.value, "Dell");
    assert_eq!(c.brand.value, "Dell");
    assert_eq!(c.model.value, "XPS 13 9340");
    assert_eq!(c.model_name.value, "XPS");
    assert_eq!(c.device_name.value, "DESKTOP-4F2K9QL");
}

#[test]
fn fr17_a_version_that_names_the_product_is_the_model_name() {
    let c = lenovo().identity();
    assert_eq!(c.manufacturer.value, "Lenovo");
    assert_eq!(c.model.value, "21HMCTO1WW");
    assert_eq!(c.model_name.value, "ThinkPad X1 Carbon Gen 11");
    assert_eq!(c.soc_manufacturer.value, "AMD");
    assert_eq!(c.soc_model.value, "Ryzen 7 7840U");
}

#[test]
fn fr17_placeholders_and_empty_values_fall_back_to_dsas_own() {
    for info in [placeholders(), ComputerInfo::default()] {
        let c = info.identity();
        assert_eq!(c.manufacturer, dsa().manufacturer);
        assert_eq!(c.manufacturer.value, "thisisthepy");
        assert_eq!(c.manufacturer.origin, Origin::Dsa);
        assert_eq!(c.brand.value, "DSA");
        assert_eq!(c.model.value, "Desktop Subsystem for Android");
        assert_eq!(c.model_name.value, "Desktop Subsystem for Android");
        assert_eq!(c.device_name.value, "");
        assert_eq!(c.soc_manufacturer.value, "");
        assert_eq!(c.soc_model.value, "");
        // Android then names the device after its model.
        let report = IdentityReport::new(&DeviceIdentity::default(), Some(&build()), &c);
        assert_eq!(report.computer.device_name, "Desktop Subsystem for Android");
        assert!(report.from_computer.is_empty());
    }
    // Each field falls back on its own: a known maker on a board with placeholders.
    let c = ComputerInfo {
        system_manufacturer: "Micro-Star International Co., Ltd.".into(),
        ..placeholders()
    }
    .identity();
    assert_eq!(c.manufacturer.value, "MSI");
    assert_eq!(c.manufacturer.origin, Origin::Computer);
    assert_eq!(c.model.origin, Origin::Dsa);
    for placeholder in [
        "To be filled by O.E.M.",
        "Default string",
        "System Product Name",
        "O.E.M.",
        "None",
        "",
        "   ",
    ] {
        assert!(is_placeholder(placeholder), "{placeholder:?}");
    }
    assert!(!is_placeholder("960XGL"));
}

#[test]
fn fr17_manufacturers_lose_their_legal_suffixes() {
    for (raw, tidy) in [
        ("SAMSUNG ELECTRONICS CO., LTD.", "Samsung"),
        ("Dell Inc.", "Dell"),
        ("LENOVO", "Lenovo"),
        ("HP Inc.", "HP"),
        ("Hewlett-Packard", "HP"),
        ("ASUSTeK COMPUTER INC.", "ASUS"),
        ("Micro-Star International Co., Ltd.", "MSI"),
        ("Acer", "Acer"),
        ("Microsoft Corporation", "Microsoft"),
        ("Apple Inc.", "Apple"),
        ("Example Devices Co., Ltd.", "Example Devices"),
        ("ACME CORPORATION", "Acme"),
    ] {
        assert_eq!(tidy_manufacturer(raw).as_deref(), Some(tidy), "{raw}");
    }
    // A brand has no spaces: a maker of two words brands with the first.
    let c = ComputerInfo {
        system_manufacturer: "Example Devices Co., Ltd.".into(),
        ..ComputerInfo::default()
    }
    .identity();
    assert_eq!(c.manufacturer.value, "Example Devices");
    assert_eq!(c.brand.value, "Example");
}

#[test]
fn fr17_the_baseboard_is_the_model_only_in_the_system_products_family() {
    assert!(same_family("960XGL", "NT960XGP-G72AG"));
    assert!(same_family("21HMCTO1WW", "21HMCTO1WW"));
    assert!(!same_family("XPS 13 9340", "0KCWT7"));
    assert!(!same_family("HP EliteBook 840 G10 Notebook PC", "8B41"));
}

#[test]
fn fr17_the_chip_is_the_cpus_maker_and_model() {
    for (vendor, name, maker, model) in [
        (
            "GenuineIntel",
            "Intel(R) Core(TM) Ultra 7 155H",
            "Intel",
            "Core Ultra 7 155H",
        ),
        (
            "AuthenticAMD",
            "AMD Ryzen 7 7840U w/ Radeon 780M Graphics",
            "AMD",
            "Ryzen 7 7840U",
        ),
        ("Apple", "Apple M3", "Apple", "M3"),
        (
            "Qualcomm Technologies Inc",
            "Snapdragon(R) X Elite - X1E78100 - Qualcomm(R) Oryon(TM) CPU",
            "Qualcomm",
            "Snapdragon X Elite",
        ),
        (
            "GenuineIntel",
            "13th Gen Intel(R) Core(TM) i7-1360P",
            "Intel",
            "Core i7-1360P",
        ),
        (
            "GenuineIntel",
            "Intel(R) Core(TM) i7-9750H CPU @ 2.60GHz",
            "Intel",
            "Core i7-9750H",
        ),
        (
            "AuthenticAMD",
            "AMD Ryzen 9 5900X 12-Core Processor",
            "AMD",
            "Ryzen 9 5900X",
        ),
    ] {
        let c = ComputerInfo {
            cpu_vendor: vendor.into(),
            cpu_name: name.into(),
            ..ComputerInfo::default()
        }
        .identity();
        assert_eq!(c.soc_manufacturer.value, maker, "{name}");
        assert_eq!(c.soc_model.value, model, "{name}");
    }
    assert_eq!(soc_model_of(""), None);
}

#[test]
fn fr17_the_users_choice_wins_over_the_computers_and_reset_restores_it() {
    let computer = galaxy_book().identity();
    let mut chosen = DeviceIdentity::default();
    chosen.set(Field::Manufacturer, "thisisthepy").unwrap();
    chosen.set(Field::Model, "My tablet").unwrap();
    // DSA's own value typed on purpose is kept, as the default is the computer's.
    assert!(!chosen.is_default());
    let report = IdentityReport::new(&chosen, Some(&build()), &computer);
    assert_eq!(report.effective.manufacturer, "thisisthepy");
    assert_eq!(report.effective.model, "My tablet");
    assert_eq!(report.effective.brand, "Samsung");
    assert_eq!(report.defaults.model, "NT960XGP-G72AG");
    assert_eq!(report.origin("model"), "computer");
    assert_eq!(report.origin("device"), "dsa");
    let props = chosen.boot_properties(Some(&build()), &computer);
    assert_eq!(
        property(&props, "ro.product.model").as_deref(),
        Some("My tablet")
    );
    assert_eq!(
        property(&props, "ro.product.manufacturer").as_deref(),
        Some("thisisthepy")
    );

    chosen.reset(Field::Model);
    chosen.reset(Field::Manufacturer);
    let report = IdentityReport::new(&chosen, Some(&build()), &computer);
    assert_eq!(report.effective.model, "NT960XGP-G72AG");
    assert_eq!(report.effective.manufacturer, "Samsung");
    // A form factor typed as its default is still no change.
    chosen.set(Field::Characteristics, "tablet").unwrap();
    assert!(chosen.is_default());
}

#[test]
fn fr17_each_boot_reads_the_computer_and_keeps_its_device_name() {
    let store = Arc::new(MemoryDeviceIdentities::default());
    let name = Arc::new(std::sync::Mutex::new("BREW-BOOK".to_string()));
    let read = {
        let name = name.clone();
        Arc::new(move || ComputerInfo {
            computer_name: name.lock().unwrap().clone(),
            ..galaxy_book()
        })
    };
    let boot = BootIdentity::new(store.clone(), "me", read);
    assert_eq!(boot.device_name(), None, "no boot yet");
    let props = boot.boot_properties(Some(&build()));
    assert_eq!(
        property(&props, "ro.product.model").as_deref(),
        Some("NT960XGP-G72AG")
    );
    assert_eq!(boot.device_name().as_deref(), Some("BREW-BOOK"));
    // The user renames the computer: the next start follows.
    *name.lock().unwrap() = "Living room".into();
    boot.boot_properties(Some(&build()));
    assert_eq!(boot.device_name().as_deref(), Some("Living room"));
    // The user's choice is read at each boot too.
    let mut mine = DeviceIdentity::default();
    mine.set(Field::Model, "My tablet").unwrap();
    store.set("me", &mine).unwrap();
    let props = boot.boot_properties(Some(&build()));
    assert_eq!(
        property(&props, "ro.product.model").as_deref(),
        Some("My tablet")
    );
}

#[test]
fn fr17_the_firmware_tables_give_the_system_board_and_processor() {
    // SMBIOS: type 1 (system), type 2 (baseboard), type 4 (processor), type 127 (end).
    let mut tables = Vec::new();
    let mut system = vec![1u8, 0x1b, 0, 1, 1, 2, 3, 0];
    system.resize(0x1b, 0);
    system[0x19] = 0; // no SKU
    system[0x1a] = 4;
    tables.extend(&system);
    tables.extend(b"SAMSUNG ELECTRONICS CO., LTD.\0960XGL\0P10ALX\0Galaxy Book4 Ultra\0\0");
    tables.extend([2u8, 8, 0, 2, 1, 2, 0, 0]);
    tables.extend(b"SAMSUNG ELECTRONICS CO., LTD.\0NT960XGP-G72AG\0\0");
    let mut processor = vec![4u8, 0x1a, 0, 3];
    processor.resize(0x1a, 0);
    processor[0x07] = 1;
    processor[0x10] = 2;
    tables.extend(&processor);
    tables.extend(b"Intel(R) Corporation\0Intel(R) Core(TM) Ultra 7 155H\0\0");
    tables.extend([127u8, 4, 0, 4, 0, 0]);
    let info = from_smbios(&tables);
    assert_eq!(info.system_manufacturer, "SAMSUNG ELECTRONICS CO., LTD.");
    assert_eq!(info.system_product, "960XGL");
    assert_eq!(info.system_version, "P10ALX");
    assert_eq!(info.system_family, "Galaxy Book4 Ultra");
    assert_eq!(info.baseboard_product, "NT960XGP-G72AG");
    assert_eq!(info.cpu_name, "Intel(R) Core(TM) Ultra 7 155H");
    assert_eq!(info.identity().soc_manufacturer.value, "Intel");
    // A table cut short reads what it can and does not panic.
    let cut = from_smbios(&tables[..20]);
    assert_eq!(cut.system_product, "");
    // The registry's values come first; the tables fill only what it lacks.
    let mut registry = ComputerInfo {
        system_manufacturer: "Samsung Electronics".into(),
        system_family: "Default string".into(),
        ..ComputerInfo::default()
    };
    registry.fill_from(&info);
    assert_eq!(registry.system_manufacturer, "Samsung Electronics");
    assert_eq!(registry.system_family, "Galaxy Book4 Ultra");
    assert_eq!(registry.baseboard_product, "NT960XGP-G72AG");
}

#[test]
fn fr17_a_changed_identity_reaches_every_partition() {
    let mut identity = DeviceIdentity::default();
    identity.set(Field::Manufacturer, "Example Corp.").unwrap();
    identity.set(Field::Brand, "example").unwrap();
    identity.set(Field::Model, "Example Tab (2026)").unwrap();
    identity.set(Field::Device, "ex_tab").unwrap();
    identity.set(Field::Name, "ex_tab_x86").unwrap();
    identity.set(Field::Characteristics, "phone").unwrap();
    let props = identity.boot_properties(Some(&build()), &dsa());

    assert_eq!(
        property(&props, "ro.product.model").as_deref(),
        Some("Example Tab (2026)")
    );
    for partition in [
        "system",
        "system_ext",
        "vendor",
        "product",
        "odm",
        "vendor_dlkm",
    ] {
        assert_eq!(
            property(&props, &format!("ro.product.{partition}.manufacturer")).as_deref(),
            Some("Example Corp.")
        );
        assert_eq!(
            property(&props, &format!("ro.product.{partition}.device")).as_deref(),
            Some("ex_tab")
        );
        assert_eq!(
            property(&props, &format!("ro.{partition}.build.fingerprint")).as_deref(),
            Some("example/ex_tab_x86/ex_tab:16/BP2A.250605.031/13690935.dsa0123456789ab:user/release-keys")
        );
    }
    assert_eq!(
        property(&props, "ro.product.brand").as_deref(),
        Some("example")
    );
    assert_eq!(
        property(&props, "ro.product.name").as_deref(),
        Some("ex_tab_x86")
    );
    assert_eq!(
        property(&props, "ro.build.characteristics").as_deref(),
        Some("phone")
    );
    assert_eq!(
        property(&props, "ro.build.product").as_deref(),
        Some("ex_tab")
    );
    assert_eq!(
        property(&props, "ro.build.flavor").as_deref(),
        Some("ex_tab_x86-user")
    );
    assert_eq!(
        property(&props, "ro.build.fingerprint").as_deref(),
        Some("example/ex_tab_x86/ex_tab:16/BP2A.250605.031/13690935.dsa0123456789ab:user/release-keys")
    );
}

#[test]
fn fr17_fingerprint_parts_and_the_whole_string_combine_as_set() {
    let mut identity = DeviceIdentity::default();
    identity.set(Field::FingerprintType, "userdebug").unwrap();
    identity.set(Field::FingerprintTags, "dev-keys").unwrap();
    identity.set(Field::FingerprintRelease, "15").unwrap();
    let e = identity.effective(Some(&build()), &dsa());
    assert_eq!(
        e.fingerprint,
        "DSA/dsa_x86_64/dsa_x86_64:15/BP2A.250605.031/13690935.dsa0123456789ab:userdebug/dev-keys"
    );

    // The whole string wins over its parts for Build.FINGERPRINT; each partition's own
    // fingerprint stays the composed one, which keeps the image's tag.
    identity
        .set(
            Field::Fingerprint,
            "google/oriole/oriole:14/UQ1A.240105.004/11206848:user/release-keys",
        )
        .unwrap();
    let props = identity.boot_properties(Some(&build()), &dsa());
    assert_eq!(
        property(&props, "ro.build.fingerprint").as_deref(),
        Some("google/oriole/oriole:14/UQ1A.240105.004/11206848:user/release-keys")
    );
    assert_eq!(
        property(&props, "ro.system.build.fingerprint").as_deref(),
        Some("DSA/dsa_x86_64/dsa_x86_64:15/BP2A.250605.031/13690935.dsa0123456789ab:userdebug/dev-keys")
    );
    // Without the image's build, only a whole string can be a fingerprint.
    let mut parts_only = DeviceIdentity::default();
    parts_only.set(Field::Brand, "example").unwrap();
    let props = parts_only.boot_properties(None, &dsa());
    assert_eq!(property(&props, "ro.build.fingerprint"), None);
    assert_eq!(
        property(&props, "ro.product.brand").as_deref(),
        Some("example")
    );
}

#[test]
fn fr17_values_that_are_not_allowed_are_refused() {
    let long_model = "x".repeat(65);
    let long_name = "n".repeat(33);
    let refused: [(Field, &str); 16] = [
        (Field::Manufacturer, " leading space"),
        (Field::Model, "line\nbreak"),
        (Field::Model, "\u{d55c}\u{ae00}"),
        (Field::Model, &long_model),
        (Field::Brand, "two words"),
        (Field::Brand, "slash/brand"),
        (Field::Device, "-starts-with-dash"),
        (Field::Name, &long_name),
        (Field::Characteristics, "Tablet"),
        (Field::Characteristics, "tablet,,phone"),
        (Field::FingerprintType, "debug"),
        (Field::FingerprintRelease, "16:1"),
        (Field::FingerprintTags, "release keys"),
        (
            Field::Fingerprint,
            "DSA/dsa_x86_64:16/ID/1:user/release-keys",
        ),
        (Field::Fingerprint, "DSA/dsa/dsa:16/ID/1:root/release-keys"),
        (
            Field::Fingerprint,
            "DSA/dsa/dsa:16/ID/1:user/release-keys:extra",
        ),
    ];
    for (field, value) in refused {
        let mut identity = DeviceIdentity::default();
        assert!(
            identity.set(field, value).is_err(),
            "{} {value:?} was taken",
            field.key()
        );
        assert!(identity.is_default(), "a refused value changed nothing");
    }
    let mut identity = DeviceIdentity::default();
    identity
        .set(Field::Characteristics, "tablet,nosdcard")
        .unwrap();
    identity
        .set(
            Field::Fingerprint,
            "DSA/dsa_x86_64/dsa_x86_64:16/BP2A.250605.031/1.dsaab:user/release-keys",
        )
        .unwrap();
    assert!(split_fingerprint("a/b/c:d/e/f:g/h").is_some());
    assert!(split_fingerprint("a/b/c:d/e/f:g").is_none());
}

#[test]
fn fr17_every_field_has_a_name_the_command_takes() {
    for field in Field::ALL {
        assert_eq!(Field::parse(field.key()).unwrap(), field);
    }
    assert!(Field::parse("serial")
        .unwrap_err()
        .contains("fingerprint.tags"));
}

#[test]
fn fr17_the_identity_is_kept_per_user_and_reset_restores_the_defaults() {
    let dir = scratch("store");
    let store = FileDeviceIdentities::new(&dir);
    let mut mine = DeviceIdentity::default();
    mine.set(Field::Model, "My tablet").unwrap();
    store.set("S-1-5-21-1", &mine).unwrap();
    assert_eq!(store.get("S-1-5-21-1").unwrap().model, "My tablet");
    assert!(
        store.get("S-1-5-21-2").unwrap().is_default(),
        "another user's is untouched"
    );
    assert!(dir.join("users").join("S-1-5-21-1.device.json").exists());

    // An identity with a value that is not allowed changes nothing.
    let broken = DeviceIdentity {
        brand: "no spaces allowed".into(),
        ..DeviceIdentity::default()
    };
    assert!(store.set("S-1-5-21-1", &broken).is_err());
    assert_eq!(store.get("S-1-5-21-1").unwrap().model, "My tablet");

    store.set("S-1-5-21-1", &DeviceIdentity::default()).unwrap();
    assert!(store.get("S-1-5-21-1").unwrap().is_default());
    assert!(!dir.join("users").join("S-1-5-21-1.device.json").exists());
    assert!(
        store.get("../escape").is_err(),
        "a user name never leaves the folder"
    );
    let _ = std::fs::remove_dir_all(&dir);

    let memory = MemoryDeviceIdentities::default();
    memory.set("me", &mine).unwrap();
    assert_eq!(memory.get("me").unwrap(), mine);
}

#[test]
fn fr17_the_identity_file_goes_in_the_initrd_before_the_bootconfig() {
    let props = "ro.product.model=My tablet\n";
    let overlay = ramdisk_overlay(props);
    let files = cpio_files(&overlay);
    let names: Vec<&str> = files.iter().map(|(name, _, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "system",
            "system/etc",
            "system/etc/ramdisk",
            RAMDISK_PROP_PATH
        ]
    );
    assert_eq!(files[0].1 & 0o170000, 0o040000, "a folder");
    assert_eq!(files[3].1, 0o100644, "a plain file init can read");
    assert_eq!(files[3].2, props.as_bytes());

    // An initrd that does not end on four bytes: the archive starts on the next boundary,
    // as the kernel needs for an uncompressed archive after a compressed one.
    let initrd = b"\x1f\x8b gzip of seven".to_vec();
    let combined = append_overlay(&initrd, &overlay);
    let start = initrd.len().div_ceil(4) * 4;
    assert_eq!(&combined[..initrd.len()], &initrd[..]);
    assert!(combined[initrd.len()..start].iter().all(|&b| b == 0));
    assert_eq!(&combined[start..], &overlay[..]);
    // The bootconfig still ends the initrd, where the kernel looks for it.
    let booted = bootconfig::append(&combined, "androidboot.example = \"1\"");
    assert!(booted.ends_with(b"#BOOTCONFIG\n"));
    assert_eq!(&booted[..combined.len()], &combined[..]);
}

#[test]
fn fr17_the_image_build_comes_from_image_json() {
    let manifest: serde_json::Value = serde_json::json!({
        "version": "17.0.0",
        "arch": "x86_64",
        "identity": {"release": "16", "id": "BP2A.250605.031",
                     "incremental": "13690935.dsa0123456789ab", "tags": "release-keys"}
    });
    assert_eq!(ImageBuild::from_manifest(&manifest), Some(build()));
    assert_eq!(
        ImageBuild::from_manifest(&serde_json::json!({"version": "1"})),
        None
    );
}
