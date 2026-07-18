use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use apb_engine::script::run_script;
use apb_engine::state::NodeStatus;

use crate::common::env_lock;

fn write_script(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, body).unwrap();
}

fn in_path(program: &str) -> bool {
    std::env::var("PATH")
        .ok()
        .map(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
        .unwrap_or(false)
}

// The extensible runner registry (spec 7.4). All phases in one test,
// sequentially: APB_CONFIG_DIR is process-global, parallel #[test]s
// would race over it.
#[test]
fn runner_registry_resolution() {
    let _env = env_lock();
    let ver = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    write_script(ver.path(), "scripts/ok.sh", "echo hello");

    let timeout = Some(Duration::from_secs(10));

    // The config directory (empty for now).
    let cfg_dir = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg_dir.path());
    }

    // 1. An unknown key with no entry in the config - "unsupported runner" error.
    let e = run_script(
        ver.path(),
        work.path(),
        "scripts/ok.sh",
        "rb",
        timeout,
        None,
    )
    .unwrap_err();
    assert!(format!("{e}").contains("unsupported runner"), "got: {e}");

    // 2. The key is defined in the config, but no runtime is available - a clear
    //    "no runtime available" error.
    let cfg = "runners:\n  rb: [definitely-not-a-real-runtime-xyz]\n  mysh: [definitely-not-a-real-runtime-xyz, sh]\n";
    fs::write(cfg_dir.path().join("config.yaml"), cfg).unwrap();
    let e = run_script(
        ver.path(),
        work.path(),
        "scripts/ok.sh",
        "rb",
        timeout,
        None,
    )
    .unwrap_err();
    assert!(format!("{e}").contains("no runtime available"), "got: {e}");

    // 3. The registry from the config adds the `mysh` key and picks the first AVAILABLE
    //    runtime (the first candidate is absent, the second is sh).
    let r = run_script(
        ver.path(),
        work.path(),
        "scripts/ok.sh",
        "mysh",
        timeout,
        None,
    )
    .unwrap();
    assert_eq!(r.status, NodeStatus::Succeeded);
    assert_eq!(r.stdout, "hello");

    // 3b. The runtime is given as an ABSOLUTE path with basename `bun`: classification must
    //     go by basename, so `run` is still passed as the first argument.
    //     The fake `bun` prints its first argument - this checks that
    //     `run` was substituted while the full path was preserved when launching.
    let bin = cfg_dir.path().join("fakebin");
    fs::create_dir_all(&bin).unwrap();
    let fake_bun = bin.join("bun");
    fs::write(&fake_bun, "#!/bin/sh\necho \"arg1=$1\"\n").unwrap();
    let mut perm = fs::metadata(&fake_bun).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&fake_bun, perm).unwrap();
    let cfg = format!("runners:\n  ts: [{}]\n", fake_bun.display());
    fs::write(cfg_dir.path().join("config.yaml"), cfg).unwrap();
    write_script(ver.path(), "scripts/ok.ts", "// noop");
    let r = run_script(
        ver.path(),
        work.path(),
        "scripts/ok.ts",
        "ts",
        timeout,
        None,
    )
    .unwrap();
    assert_eq!(r.status, NodeStatus::Succeeded);
    assert_eq!(
        r.stdout, "arg1=run",
        "full-path bun must still receive `run` first arg"
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }

    // 4. The ts runner through the default registry (bun) - only if bun is installed,
    //    otherwise skip so it doesn't flap in CI without the runtime.
    if in_path("bun") {
        write_script(ver.path(), "scripts/ok.ts", "console.log('ts-ok')");
        let r = run_script(
            ver.path(),
            work.path(),
            "scripts/ok.ts",
            "ts",
            timeout,
            None,
        )
        .unwrap();
        assert_eq!(r.status, NodeStatus::Succeeded);
        assert!(r.stdout.contains("ts-ok"));
    }
}
