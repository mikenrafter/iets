//! `file` inputs: a flake input locked as `{ type = "file"; url; narHash; }`
//! and `builtins.fetchTree { type = "file"; ... }`, against the store paths
//! and messages of Nix 2.34.8 for the same bytes. Nix copies the file to the
//! store as a NAR named `source`, so the lock's `narHash` names the path,
//! as it does for a tarball.
//!
//! Every URL is a `file://` URL into a scratch directory: nothing here needs
//! the network, a daemon or a nix.

mod hermetic;

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fs};

use iets::store::hash::{Hash, HashAlgo, Hasher};
use iets::store::nar;
use iets::treestore;

/// The input's bytes: a Nix expression, so `import` reads it too.
const BYTES: &[u8] = b"{ x = 42; }\n";
/// `nix hash path` of `BYTES` in a file.
const NAR_HASH: &str = "sha256-MdxHg1ku5IMYlYQJKViWE6jQEIMeXnBBdhIQX2sg9j8=";
/// `nix eval --raw .#path` with the input locked by `nix flake lock`, and
/// `(builtins.fetchTree { type = "file"; ... }).outPath`.
const STORE_PATH: &str = "/nix/store/jpzmbw1a0g0j4fgnpl2lj9kl7vw3ignc-source";

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The `file://` URL of a path. Windows drives come after a third slash.
fn file_url(p: &Path) -> String {
    let p = p.display().to_string().replace('\\', "/");
    if p.starts_with('/') {
        format!("file://{p}")
    } else {
        format!("file:///{p}")
    }
}

/// A scratch directory holding `input.nix` with `bytes`, and a cache
/// directory beside it.
fn scratch(name: &str, bytes: &[u8]) -> (PathBuf, PathBuf) {
    let d = env::temp_dir().join(format!("iets-file-input-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("input.nix"), bytes).unwrap();
    let cache = d.join("cache");
    (d, cache)
}

/// A flake in `dir/flake` with one input `f`, written as a user writes it
/// (`url` and `flake = false`), and the `flake.lock` that `nix flake lock`
/// (2.34.8) writes for it, with `nar_hash` as the locked hash.
fn flake(dir: &Path, nar_hash: &str) -> PathBuf {
    let url = file_url(&dir.join("input.nix"));
    let f = dir.join("flake");
    fs::create_dir_all(&f).unwrap();
    fs::write(
        f.join("flake.nix"),
        format!(
            r#"{{
  inputs.f = {{ url = "{url}"; flake = false; }};
  outputs = {{ self, f }}: {{
    path = f.outPath;
    text = builtins.readFile f;
    x = (import f).x;
  }};
}}
"#
        ),
    )
    .unwrap();
    fs::write(
        f.join("flake.lock"),
        format!(
            r#"{{
  "nodes": {{
    "f": {{
      "flake": false,
      "locked": {{
        "narHash": "{nar_hash}",
        "type": "file",
        "url": "{url}"
      }},
      "original": {{
        "type": "file",
        "url": "{url}"
      }}
    }},
    "root": {{
      "inputs": {{
        "f": "f"
      }}
    }}
  }},
  "root": "root",
  "version": 7
}}
"#
        ),
    )
    .unwrap();
    f
}

fn eval(cache: &Path, offline: bool, args: &[&str]) -> Output {
    let mut c = hermetic::iets();
    c.arg("eval")
        .args(args)
        .env("IETS_CACHE_DIR", cache)
        .env("IETS_CACHE", "0");
    if offline {
        c.env("IETS_OFFLINE", "1");
    }
    c.output().expect("run iets")
}

fn ok(o: &Output) -> String {
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    text(&o.stdout)
}

/// The NAR hash of a single regular file holding `bytes`.
fn nar_hash_of_string(bytes: &[u8]) -> Hash {
    let mut h = Hasher::new(HashAlgo::Sha256);
    nar::dump_string(bytes, &mut h).unwrap();
    h.finish()
}

fn file_nar_hash(bytes: &[u8]) -> String {
    nar_hash_of_string(bytes).to_sri()
}

#[test]
fn the_fixture_hash_is_the_nar_hash_of_one_regular_file() {
    // guards the constants below against a typo: Nix hashes a file input's
    // NAR, not its bytes
    assert_eq!(file_nar_hash(BYTES), NAR_HASH);
}

#[test]
fn a_locked_file_input_has_the_store_path_nix_gives() {
    let (d, cache) = scratch("flake", BYTES);
    let f = flake(&d, NAR_HASH);
    let r = |attr: &str| format!("{}#{attr}", f.display());

    assert_eq!(ok(&eval(&cache, false, &["--raw", &r("path")])), STORE_PATH);
    assert_eq!(
        ok(&eval(&cache, false, &["--raw", &r("text")])).as_bytes(),
        BYTES
    );
    assert_eq!(ok(&eval(&cache, false, &[&r("x")])).trim_end(), "42");
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn fetch_tree_of_a_file_has_the_store_path_and_attributes_nix_gives() {
    let (d, cache) = scratch("fetchtree", BYTES);
    let url = file_url(&d.join("input.nix"));
    let call = format!(
        r#"builtins.fetchTree {{ type = "file"; url = "{url}"; narHash = "{NAR_HASH}"; }}"#
    );

    let o = eval(
        &cache,
        false,
        &["--raw", "--expr", &format!("({call}).outPath")],
    );
    assert_eq!(ok(&o), STORE_PATH);
    // `nix eval --json` of the attribute names: a file has no lastModified
    let o = eval(
        &cache,
        false,
        &["--json", "--expr", &format!("builtins.attrNames ({call})")],
    );
    assert_eq!(ok(&o).trim_end(), r#"["narHash","outPath"]"#);
    let o = eval(
        &cache,
        false,
        &["--raw", "--expr", &format!("builtins.readFile ({call})")],
    );
    assert_eq!(ok(&o).as_bytes(), BYTES);
    let o = eval(&cache, false, &["--expr", &format!("(import ({call})).x")]);
    assert_eq!(ok(&o).trim_end(), "42");
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_file_whose_bytes_are_not_the_locked_ones_is_refused_naming_both_hashes() {
    let (d, cache) = scratch("mismatch", b"not what the lock says\n");
    let got = file_nar_hash(b"not what the lock says\n");
    let url = file_url(&d.join("input.nix"));

    // fetchTree: Nix says
    // NAR hash mismatch in input '<url>?narHash=...', expected '<lock>' but got '<file>'
    let call = format!(
        r#"(builtins.fetchTree {{ type = "file"; url = "{url}"; narHash = "{NAR_HASH}"; }}).outPath"#
    );
    let o = eval(&cache, false, &["--raw", "--expr", &call]);
    let err = text(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(o.stdout.is_empty(), "{}", text(&o.stdout));
    assert!(err.contains("NAR hash mismatch in input"), "{err}");
    assert!(
        err.contains(&format!("expected '{NAR_HASH}' but got '{got}'")),
        "{err}"
    );

    // the same bytes behind a flake.lock
    let f = flake(&d, NAR_HASH);
    let o = eval(&cache, false, &["--raw", &format!("{}#text", f.display())]);
    let err = text(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(o.stdout.is_empty(), "{}", text(&o.stdout));
    assert!(err.contains(NAR_HASH) && err.contains(&got), "{err}");

    // nothing was kept under the path the lock names
    let name = STORE_PATH.rsplit('/').next().unwrap();
    assert!(!cache.join("inputs").join(name).exists());
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_fetched_file_input_needs_no_network_the_second_time() {
    // bytes no store has yet, so the first run has to download them
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let bytes = format!(
        "{{ x = 42; nonce = \"{nonce}-{}\"; }}\n",
        std::process::id()
    );
    let (d, cache) = scratch("offline", bytes.as_bytes());
    let hash = nar_hash_of_string(bytes.as_bytes());
    let want = treestore::store_path("source", &hash)
        .unwrap()
        .to_full_string();
    let f = flake(&d, &hash.to_sri());
    let r = |attr: &str| format!("{}#{attr}", f.display());

    let o = eval(&cache, false, &["--raw", &r("path")]);
    assert_eq!(ok(&o), want);
    let err = text(&o.stderr);
    assert!(
        err.contains("is not in the store"),
        "the first run fetched: {err}"
    );

    // the URL is gone and downloads are off: the input store has it
    fs::remove_file(d.join("input.nix")).unwrap();
    assert_eq!(ok(&eval(&cache, true, &["--raw", &r("path")])), want);
    assert_eq!(ok(&eval(&cache, true, &["--raw", &r("text")])), bytes);
    assert_eq!(ok(&eval(&cache, true, &[&r("x")])).trim_end(), "42");

    // fetchTree of the same locked file is the same entry
    let call = format!(
        r#"builtins.readFile (builtins.fetchTree {{ type = "file"; url = "{}"; narHash = "{}"; }})"#,
        file_url(&d.join("input.nix")),
        hash.to_sri()
    );
    assert_eq!(ok(&eval(&cache, true, &["--raw", "--expr", &call])), bytes);
    fs::remove_dir_all(&d).unwrap();
}
