//! The built application starting the embedded Chromium by itself, as the
//! window would and without one: its own executable runs CEF's helper
//! processes, CEF's library and resources are found the way a launch finds
//! them, and a normal exit leaves no process and no directory. Opt-in:
//!
//! ```sh
//! nix-shell --run "cargo test -p agents-window -- --ignored real_cef --test-threads=1"
//! ```
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The command lines of every process that names `directory`.
fn processes_using(directory: &str) -> Vec<String> {
    std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_str().is_some_and(|name| name.bytes().all(|byte| byte.is_ascii_digit())))
        .filter_map(|entry| std::fs::read(entry.path().join("cmdline")).ok())
        .map(|command| String::from_utf8_lossy(&command).replace('\0', " "))
        .filter(|command| command.contains(directory))
        .collect()
}

#[test]
#[ignore = "opt-in: starts the embedded Chromium, windowless"]
fn real_cef_probe_starts_in_the_application_itself_and_leaves_nothing_behind() {
    let probe = Command::new(env!("CARGO_BIN_EXE_agents-window"))
        .arg("--cef-probe")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let process = probe.id();
    let output = probe.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{said}{}", String::from_utf8_lossy(&output.stderr));
    assert!(said.starts_with("CEF funciona: primer cuadro de 2560 × 1600 "), "{said}");
    let directory = said
        .lines()
        .find_map(|line| line.strip_prefix("Directorio temporal de CEF: "))
        .expect("the probe says where CEF kept its files");

    // CEF's helpers carry its directory on their command lines. They end
    // with the process that started them, which removed the directory.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !processes_using(directory).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(processes_using(directory), Vec::<String>::new());
    assert!(!Path::new(directory).exists(), "{directory} is still there");
    let data = std::env::temp_dir().join(format!("openide-cef-probe-{process}"));
    assert!(!data.exists(), "{data:?} is still there");
}

#[test]
#[ignore = "opt-in: runs the application's probe with CEF's data files missing"]
fn real_cef_that_cannot_start_says_why_and_leaves_a_running_application() {
    // CEF's library alone, without the data files it reads from beside it:
    // an installation CEF itself would abort the whole process on.
    let search = std::env::var_os("LD_LIBRARY_PATH").expect("the shell sets the library path");
    let library = std::env::split_paths(&search)
        .map(|directory| directory.join("libcef.so"))
        .find(|library| library.is_file())
        .expect("libcef.so is on the library path");
    let broken = std::env::temp_dir().join(format!("openide-cef-broken-{}", std::process::id()));
    std::fs::create_dir(&broken).unwrap();
    std::os::unix::fs::symlink(&library, broken.join("libcef.so")).unwrap();
    let mut first = vec![PathBuf::from(&broken)];
    first.extend(std::env::split_paths(&search));
    let output = Command::new(env!("CARGO_BIN_EXE_agents-window"))
        .arg("--cef-probe")
        .env("LD_LIBRARY_PATH", std::env::join_paths(first).unwrap())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    std::fs::remove_dir_all(&broken).unwrap();
    let said = String::from_utf8_lossy(&output.stderr);
    // The probe ends by itself, saying so: nothing killed it.
    assert_eq!(output.status.code(), Some(1), "{said}");
    assert!(said.contains("openADE: el navegador embebido (CEF) no pudo iniciarse. Falta icudtl.dat junto a "), "{said}");
    // There is no other browser to start: the context failed, with what its
    // tab says in the reason's place and how CEF may be tried again.
    let told = said.lines().last().unwrap_or_default();
    assert!(told.starts_with("Falta icudtl.dat junto a "), "{said}");
    assert!(told.ends_with("se inicia una sola vez por ejecución: cerrá openADE y volvé a abrirlo para reintentar."), "{said}");
}
