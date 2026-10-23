use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn dsa_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn nfr7_every_rust_crate_complies_with_licence_policy() {
    let dsa_dir = dsa_root();
    let deny_path = dsa_dir.join("deny.toml");
    let deny_content = std::fs::read_to_string(&deny_path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", deny_path.display()));

    let mut allowed: HashSet<String> = HashSet::new();
    for line in deny_content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('"') && trimmed.ends_with("\",") {
            let lic = trimmed.trim_matches('"').trim_end_matches("\",");
            allowed.insert(lic.to_string());
        }
    }
    assert!(!allowed.is_empty(), "allow list in deny.toml is empty");

    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1"])
        .current_dir(&dsa_dir)
        .output()
        .expect("cargo metadata execution");
    assert!(output.status.success(), "cargo metadata failed");

    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid metadata json");
    let packages = metadata["packages"].as_array().expect("packages array");

    let mut violating = Vec::new();
    for pkg in packages {
        let name = pkg["name"].as_str().unwrap();
        let version = pkg["version"].as_str().unwrap();
        let license_expr = pkg["license"].as_str().unwrap_or("UNKNOWN");

        // Simple evaluation: split by OR and AND
        let tokens: Vec<&str> = license_expr
            .split(['/', '(', ')'])
            .flat_map(|part| part.split(" OR "))
            .flat_map(|part| part.split(" AND "))
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();

        // At least one permitted license must be present if expression is an OR-combination
        let any_allowed = tokens.iter().any(|t| allowed.contains(*t));
        if !any_allowed {
            violating.push(format!("{name} {version}: {license_expr}"));
        }
    }

    assert!(
        violating.is_empty(),
        "Dependencies violating licence policy:\n{}",
        violating.join("\n")
    );
}

#[test]
fn nfr7_licence_bundle_lists_every_rust_crate_in_cargo_lock() {
    let dsa_dir = dsa_root();
    let lock_path = dsa_dir.join("Cargo.lock");
    let lock_content = std::fs::read_to_string(&lock_path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", lock_path.display()));

    let mut lock_crates = HashSet::new();
    for line in lock_content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("name = \"") {
            if let Some(crate_name) = rest.strip_suffix('"') {
                lock_crates.insert(crate_name.to_string());
            }
        }
    }
    assert!(!lock_crates.is_empty(), "Cargo.lock has no packages");

    let temp_dir = std::env::temp_dir().join(format!("nfr7-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp_dir);
    let out_dir = temp_dir.join("licenses");
    let master_notice = temp_dir.join("NOTICE.txt");
    let script_path = dsa_dir.join("host/platform/windows/installer/generate-licenses.py");

    let python = if Command::new("python3").arg("--version").output().is_ok() {
        "python3"
    } else {
        "python"
    };

    let status = Command::new(python)
        .args([
            script_path.to_str().unwrap(),
            "--workspace",
            dsa_dir.to_str().unwrap(),
            "--out-dir",
            out_dir.to_str().unwrap(),
            "--master-notice",
            master_notice.to_str().unwrap(),
        ])
        .status()
        .expect("running generate-licenses.py");
    assert!(status.success(), "generate-licenses.py failed");

    let crates_file = out_dir.join("rust-crates.txt");
    assert!(crates_file.exists(), "rust-crates.txt was not created");
    let bundle_content = std::fs::read_to_string(&crates_file).expect("reading rust-crates.txt");

    let mut missing = Vec::new();
    for crate_name in &lock_crates {
        let prefix = format!("{crate_name} ");
        if !bundle_content.contains(&prefix) {
            missing.push(crate_name.as_str());
        }
    }

    let _ = std::fs::remove_dir_all(&temp_dir);

    assert!(
        missing.is_empty(),
        "Licence bundle missing crates from Cargo.lock: {:?}",
        missing
    );
}

#[test]
fn nfr7_no_wsa_binaries_in_payload_or_image_inputs() {
    let dsa_dir = dsa_root();
    let inputs_path = dsa_dir.join("guest/image/inputs.json");
    let inputs_content = std::fs::read_to_string(&inputs_path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", inputs_path.display()));

    let lower = inputs_content.to_lowercase();
    assert!(
        !lower.contains("windows subsystem for android"),
        "inputs.json contains forbidden WSA text"
    );
    assert!(
        !lower.contains("wsaclient"),
        "inputs.json contains wsaclient"
    );
    assert!(
        !lower.contains("wsaservice"),
        "inputs.json contains wsaservice"
    );

    let wxs_path = dsa_dir.join("host/platform/windows/installer/dsa.wxs");
    let wxs_content = std::fs::read_to_string(&wxs_path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", wxs_path.display()));
    let wxs_lower = wxs_content.to_lowercase();
    assert!(
        !wxs_lower.contains("wsaclient"),
        "dsa.wxs contains wsaclient"
    );
    assert!(
        !wxs_lower.contains("wsaservice"),
        "dsa.wxs contains wsaservice"
    );
    assert!(
        !wxs_lower.contains("wsapackage"),
        "dsa.wxs contains wsapackage"
    );
}
