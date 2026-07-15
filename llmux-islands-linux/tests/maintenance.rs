use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::PathBuf;

use llmux_islands_linux::maintenance::{
    classify_install_owner, install_verified_artifact, plan_maintenance, InstallEvidence,
    InstallOwner, MaintenanceDisposition, MaintenanceIntent,
};

fn evidence(executable: &str) -> InstallEvidence {
    InstallEvidence {
        executable: PathBuf::from(executable),
        home_dir: Some(PathBuf::from("/home/alice")),
        pacman_package: None,
        homebrew_prefixes: vec![PathBuf::from("/home/linuxbrew/.linuxbrew")],
    }
}

#[test]
fn pacman_ownership_wins_and_never_plans_an_in_place_write() {
    let mut input = evidence("/usr/bin/llmux-islands-linux");
    input.pacman_package = Some("llmux-islands-git".to_string());

    let owner = classify_install_owner(&input);
    assert_eq!(
        owner,
        InstallOwner::Pacman {
            package: "llmux-islands-git".to_string()
        }
    );

    let report = plan_maintenance(&owner, MaintenanceIntent::Update);
    assert_eq!(report.disposition, MaintenanceDisposition::Instruction);
    assert!(
        report.command.is_none(),
        "the app must not invoke sudo/pacman"
    );
    assert!(report.message.contains("pacman -Syu llmux-islands-git"));
    assert!(report.message.contains("No files were changed"));
}

#[test]
fn channel_change_is_package_aware_and_confirmation_safe() {
    let owner = InstallOwner::Pacman {
        package: "llmux-islands-git".to_string(),
    };

    let stable = plan_maintenance(&owner, MaintenanceIntent::ChangeChannel("stable".into()));
    let preview = plan_maintenance(&owner, MaintenanceIntent::ChangeChannel("preview".into()));

    assert_eq!(stable.disposition, MaintenanceDisposition::Instruction);
    assert_eq!(preview.disposition, MaintenanceDisposition::Instruction);
    assert!(stable.message.contains("llmux-islands"));
    assert!(preview.message.contains("llmux-islands-preview"));
    assert!(stable.command.is_none());
    assert!(preview.command.is_none());
}

#[test]
fn ownership_classification_distinguishes_homebrew_self_managed_and_unknown() {
    assert_eq!(
        classify_install_owner(&evidence(
            "/home/linuxbrew/.linuxbrew/Cellar/llmux-islands/0.2.16/bin/llmux-islands-linux"
        )),
        InstallOwner::Homebrew
    );
    assert_eq!(
        classify_install_owner(&evidence("/home/alice/.local/bin/llmux-islands-linux")),
        InstallOwner::SelfManaged
    );
    assert_eq!(
        classify_install_owner(&evidence("/opt/custom/llmux-islands-linux")),
        InstallOwner::Unknown
    );
}

#[test]
fn homebrew_plan_contains_only_arguments_for_an_absolute_resolved_cli() {
    let report = plan_maintenance(&InstallOwner::Homebrew, MaintenanceIntent::Update);

    assert_eq!(report.disposition, MaintenanceDisposition::Completed);
    assert_eq!(report.command, Some(vec!["update".into()]));
    assert_ne!(
        report.command.as_ref().and_then(|parts| parts.first()),
        Some(&"llmux".into()),
        "production must never PATH-resolve the credential-bearing CLI"
    );
}

#[test]
fn verified_installer_rejects_wrong_digest_before_touching_destination() {
    let root = unique_temp_dir("wrong-digest");
    fs::create_dir_all(&root).expect("create temp root");
    let destination = root.join("bin/llmux-islands-linux");

    let error = install_verified_artifact(
        b"new executable",
        "0000000000000000000000000000000000000000000000000000000000000000",
        &destination,
        &root,
    )
    .expect_err("a mismatched digest must fail closed");

    assert!(error.to_string().contains("checksum"));
    assert!(!destination.exists());
    fs::remove_dir_all(root).expect("remove test root");
}

#[test]
fn verified_installer_atomically_writes_only_below_the_user_root() {
    let root = unique_temp_dir("verified");
    fs::create_dir_all(&root).expect("create temp root");
    let destination = root.join("bin/llmux-islands-linux");
    // SHA-256("new executable")
    let digest = "c9b6741bedeb8efb2de410ac471b80eb2fb9286df430d4d03ab1c7cb4408e2e5";

    install_verified_artifact(b"new executable", digest, &destination, &root)
        .expect("verified user-owned install");

    assert_eq!(
        fs::read(&destination).expect("read install"),
        b"new executable"
    );
    assert_eq!(
        fs::metadata(&destination)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert!(
        fs::read_dir(destination.parent().expect("bin parent"))
            .expect("read bin")
            .all(|entry| !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .contains(".tmp")),
        "atomic temp file must not remain"
    );

    let outside = root.parent().expect("temp parent").join("outside-llmux");
    let error = install_verified_artifact(b"new executable", digest, &outside, &root)
        .expect_err("writes outside the user-owned root must fail");
    assert!(error.to_string().contains("user-owned"));

    fs::remove_dir_all(root).expect("remove test root");
}

#[test]
fn verified_installer_rejects_system_roots_and_symlinked_path_components() {
    let digest = "c9b6741bedeb8efb2de410ac471b80eb2fb9286df430d4d03ab1c7cb4408e2e5";
    let system_error = install_verified_artifact(
        b"new executable",
        digest,
        PathBuf::from("/tmp/llmux-islands-should-not-exist").as_path(),
        PathBuf::from("/").as_path(),
    )
    .expect_err("system roots must be rejected even for the current uid");
    assert!(system_error.to_string().contains("owned"));

    let root = unique_temp_dir("symlink-component");
    let real = root.join("real");
    fs::create_dir_all(&real).expect("real directory");
    symlink(&real, root.join("link")).expect("symlinked path component");
    let destination = root.join("link/bin/llmux-islands-linux");

    let error = install_verified_artifact(b"new executable", digest, &destination, &root)
        .expect_err("symlinked path components must fail closed");
    assert!(error.to_string().contains("symlink"));
    assert!(!real.join("bin/llmux-islands-linux").exists());
    fs::remove_dir_all(root).expect("remove test root");
}

fn unique_temp_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "llmux-islands-maintenance-{label}-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ))
}
