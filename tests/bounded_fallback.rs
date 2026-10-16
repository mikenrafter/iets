//! Bounded fallback: what iets cannot do in-process is handed off for that one
//! operation, and the evaluation goes on in iets. An import from derivation
//! realises its output through the daemon; a fetch iets has no fetcher for
//! asks Nix for that one fetch. Only what cannot be bounded, such as a flag
//! iets does not implement, still runs the real Nix for the whole command.
//!
//! The tests that build or fetch through Nix need the Nix daemon at
//! `/nix/var/nix/daemon-socket/socket` and a real nix (not iets) where
//! `iets::nixbin::real` finds it, as `nix develop` provides; without them
//! they skip themselves, as `tests/nar_parity.rs` does. They write a few
//! small paths to the store. A unique nonce in each derivation keeps one run
//! from seeing another's output.

mod hermetic;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fs};

const SOCKET: &str = "/nix/var/nix/daemon-socket/socket";

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The real nix, when the daemon is there too.
fn daemon_and_nix() -> Option<PathBuf> {
    if !Path::new(SOCKET).exists() {
        eprintln!("skipping: no nix daemon at {SOCKET}");
        return None;
    }
    let nix = iets::nixbin::real("nix");
    if nix.is_none() {
        eprintln!("skipping: no real nix on PATH");
    }
    nix
}

fn scratch(name: &str) -> PathBuf {
    let d = env::temp_dir().join(format!("iets-bounded-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn nonce(tag: &str) -> String {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{tag}-{t}-{}", std::process::id())
}

/// A derivation whose output is the Nix file `{ answer = 42; }`. The nonce
/// makes its output a path no store has yet.
fn drv(nonce: &str) -> String {
    format!(
        r#"(derivation {{
  name = "iets-ifd";
  system = builtins.currentSystem;
  builder = "/bin/sh";
  args = [ "-c" "echo '{{ answer = 42; }}' > $out" ];
  nonce = "{nonce}";
}})"#
    )
}

/// `iets ARGS` with its caches in `cache`; `IETS_CACHE=0` unless `tracing`.
fn iets(cache: &Path, tracing: bool, args: &[&str]) -> Output {
    let mut c = hermetic::iets();
    c.args(args).env("IETS_CACHE_DIR", cache);
    if !tracing {
        c.env("IETS_CACHE", "0");
    }
    c.output().expect("run iets")
}

/// `iets ARGS` under a classic name with the whole-command fallback off, so
/// a run that would fall back fails instead of running the real nix.
fn classic_no_fallback(cache: &Path, args: &[&str]) -> Output {
    let mut c = hermetic::iets();
    c.args(args)
        .env("IETS_CACHE_DIR", cache)
        .env("IETS_CACHE", "0")
        .env("IETS_NO_FALLBACK", "1");
    c.output().expect("run iets")
}

fn ok(o: &Output) -> String {
    let err = text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert!(!err.contains("falling back"), "{err}");
    text(&o.stdout)
}

fn raw(cache: &Path, expr: &str) -> String {
    ok(&iets(
        cache,
        false,
        &["eval", "--impure", "--raw", "--expr", expr],
    ))
}

fn valid(nix: &Path, path: &str) -> bool {
    let store = nix.with_file_name("nix-store");
    Command::new(store)
        .args(["--check-validity", path])
        .output()
        .is_ok_and(|o| o.status.success())
}

// --- import from derivation

#[test]
fn import_from_derivation_builds_the_output_and_goes_on_in_process() {
    let Some(nix) = daemon_and_nix() else { return };
    let d = scratch("ifd-build");
    let cache = d.join("cache");
    let drv = drv(&nonce("build"));
    let out = raw(&cache, &format!("{drv}.outPath"));
    assert!(
        !valid(&nix, &out),
        "{out} is valid before the test built it"
    );

    let o = iets(
        &cache,
        false,
        &[
            "eval",
            "--impure",
            "--expr",
            &format!("(import {drv}).answer"),
        ],
    );
    assert_eq!(ok(&o).trim_end(), "42");
    assert!(valid(&nix, &out), "{out} was not realised");

    // the other readers of a path realise it the same way
    assert_eq!(
        raw(&cache, &format!("builtins.readFile {drv}")),
        "{ answer = 42; }\n"
    );
    let o = iets(
        &cache,
        false,
        &[
            "eval",
            "--impure",
            "--expr",
            &format!("builtins.pathExists {drv}"),
        ],
    );
    assert_eq!(ok(&o).trim_end(), "true");

    // under the classic name it is no whole-command fallback either
    let o = classic_no_fallback(
        &cache,
        &[
            "nix",
            "eval",
            "--impure",
            "--expr",
            &format!("(import {drv}).answer"),
        ],
    );
    assert_eq!(ok(&o), "42\n");
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn import_from_derivation_reads_an_output_already_in_the_store() {
    let Some(nix) = daemon_and_nix() else { return };
    let d = scratch("ifd-valid");
    let cache = d.join("cache");
    let drv = drv(&nonce("valid"));
    let link = d.join("result");
    let o = iets(
        &cache,
        false,
        &[
            "build",
            "--impure",
            "-o",
            link.to_str().unwrap(),
            "-E",
            &drv,
        ],
    );
    let out = ok(&o).trim_end().to_string();
    assert!(valid(&nix, &out), "{out}");

    let o = iets(
        &cache,
        false,
        &[
            "eval",
            "--impure",
            "--expr",
            &format!("(import {drv}).answer"),
        ],
    );
    assert_eq!(ok(&o).trim_end(), "42");
    let o = classic_no_fallback(
        &cache,
        &[
            "nix-instantiate",
            "--eval",
            "-E",
            &format!("(import {drv}).answer"),
        ],
    );
    assert_eq!(ok(&o), "42\n");

    // `allow-import-from-derivation = false` refuses even a valid output, as
    // Nix does
    let drv_path = raw(&cache, &format!("{drv}.drvPath"));
    let o = iets(
        &cache,
        false,
        &[
            "eval",
            "--impure",
            "--option",
            "allow-import-from-derivation",
            "false",
            "--expr",
            &format!("import {drv}"),
        ],
    );
    let err = text(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        err.contains(&format!(
            "cannot build '{drv_path}^out' during evaluation because the option 'allow-import-from-derivation' is disabled"
        )),
        "{err}"
    );
    fs::remove_dir_all(&d).unwrap();
}

/// Nix's message for an import from derivation with the setting off, from
/// `nix eval --option allow-import-from-derivation false` (2.34.8). The
/// refusal comes before any store is asked, so this needs no daemon.
#[test]
fn import_from_derivation_off_is_nix_error() {
    let d = scratch("ifd-off");
    let cache = d.join("cache");
    let drv = drv(&nonce("off"));
    let drv_path = raw(&cache, &format!("{drv}.drvPath"));
    let want = format!(
        "cannot build '{drv_path}^out' during evaluation because the option 'allow-import-from-derivation' is disabled"
    );
    for flags in [
        &[
            "eval",
            "--impure",
            "--option",
            "allow-import-from-derivation",
            "false",
        ][..],
        &[
            "nix",
            "eval",
            "--impure",
            "--option",
            "allow-import-from-derivation",
            "false",
        ],
    ] {
        let mut args = flags.to_vec();
        let e = format!("builtins.readFile {drv}");
        args.extend(["--expr", &e]);
        let o = classic_no_fallback(&cache, &args);
        let err = text(&o.stderr);
        assert_eq!(o.status.code(), Some(1), "{flags:?}: {err}");
        assert!(o.stdout.is_empty(), "{flags:?}");
        assert!(err.contains(&want), "{flags:?}: {err}");
        assert!(!err.contains("falling back"), "{flags:?}: {err}");
    }
    fs::remove_dir_all(&d).unwrap();
}

/// The tracing cache replays an evaluation that imported from a derivation,
/// and the realised output is in its read set: once the output is gone, the
/// next run evaluates again and realises it again.
#[test]
fn the_tracing_cache_keeps_an_import_from_derivation_until_its_output_goes() {
    let Some(nix) = daemon_and_nix() else { return };
    let d = scratch("ifd-cache");
    let cache = d.join("cache");
    let drv = drv(&nonce("cache"));
    let out = raw(&cache, &format!("{drv}.outPath"));
    let expr = format!(r#"builtins.trace "evaluated" (import {drv}).answer"#);
    let run = || iets(&cache, true, &["eval", "--impure", "--expr", &expr]);

    let first = run();
    assert_eq!(ok(&first).trim_end(), "42");
    assert!(text(&first.stderr).contains("trace: evaluated"));

    let hit = run();
    assert_eq!(ok(&hit).trim_end(), "42");
    assert!(
        !text(&hit.stderr).contains("trace: evaluated"),
        "the second run evaluated again: {}",
        text(&hit.stderr)
    );

    let store = nix.with_file_name("nix-store");
    let del = Command::new(store)
        .args(["--delete", &out])
        .output()
        .expect("run nix-store");
    assert!(del.status.success(), "{}", text(&del.stderr));
    assert!(!valid(&nix, &out));

    let miss = run();
    assert_eq!(ok(&miss).trim_end(), "42");
    assert!(
        text(&miss.stderr).contains("trace: evaluated"),
        "the output was deleted and the run replayed: {}",
        text(&miss.stderr)
    );
    assert!(valid(&nix, &out), "{out} was not realised again");
    fs::remove_dir_all(&d).unwrap();
}

// --- a fetch iets has no fetcher for

/// `tests/data/tarball/tree.tar.gz`, fetched with no `narHash`: Nix 2.34.8
/// gives this path for `fetchTree { type = "tarball"; url; }` and
/// `fetchTarball url` in an impure evaluation.
const TARBALL_PATH: &str = "/nix/store/a9zisp1ips30jzqz6dxnyigi9dqjwkij-source";

#[test]
fn an_unlocked_tarball_is_one_bounded_fetch() {
    if daemon_and_nix().is_none() {
        return;
    }
    let d = scratch("tarball");
    let cache = d.join("cache");
    let tb = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/tarball/tree.tar.gz");
    let url = format!("file://{}", tb.display());
    for call in [
        format!(r#"builtins.fetchTree {{ type = "tarball"; url = "{url}"; }}"#),
        format!(r#"builtins.fetchTarball "{url}""#),
    ] {
        let path = format!("({call}).outPath");
        let file = format!(r#"builtins.readFile "${{{call}}}/sub/data""#);
        assert_eq!(raw(&cache, &path), TARBALL_PATH, "{call}");
        assert_eq!(raw(&cache, &file), "nested\n", "{call}");
        let o = classic_no_fallback(
            &cache,
            &["nix", "eval", "--impure", "--raw", "--expr", &file],
        );
        assert_eq!(ok(&o), "nested\n", "{call}");
    }
    fs::remove_dir_all(&d).unwrap();
}

// --- what cannot be bounded

#[test]
fn an_unimplemented_flag_still_falls_back_for_the_whole_command() {
    let d = scratch("whole");
    let o = classic_no_fallback(&d.join("cache"), &["nix-instantiate", "--parse", "-E", "1"]);
    let err = text(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        err.contains("iets: falling back to nix-instantiate:"),
        "{err}"
    );
    assert!(
        err.contains("iets: no-fallback is set, not running nix-instantiate"),
        "{err}"
    );
    fs::remove_dir_all(&d).unwrap();
}
