//! The ACP registry: the agents an ACP client can offer, published by the
//! protocol's maintainers, and how to run each one on this machine.

use crate::AgentCommand;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const REGISTRY_URL: &str = "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json";
/// The index changes a few times a week; a day-old copy is fresh enough.
const CACHE_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const CACHE_FILE: &str = "acp-registry.json";

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Registry {
    pub agents: Vec<RegistryAgent>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct RegistryAgent {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub website: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub distribution: Distribution,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Distribution {
    /// By platform, such as `linux-x86_64`.
    #[serde(default)]
    pub binary: BTreeMap<String, BinaryTarget>,
    #[serde(default)]
    pub npx: Option<PackageTarget>,
    #[serde(default)]
    pub uvx: Option<PackageTarget>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct BinaryTarget {
    pub archive: String,
    /// Relative to where the archive is unpacked.
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct PackageTarget {
    /// With its exact version, as in `@google/gemini-cli@0.64.0`.
    pub package: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// How an agent can run here.
#[derive(Clone, Debug, PartialEq)]
pub enum Launch {
    /// The user's own install speaks ACP already.
    Local(AgentCommand),
    /// Through `npx` or `uvx`, which fetch the pinned package on first use.
    Package(AgentCommand),
    /// The vendor's build for this platform, to download into `dir` first.
    Binary { target: BinaryTarget, dir: PathBuf },
}

/// CLIs installed from npm or PyPI whose own command serves ACP. Agents
/// shipped as binaries need no entry: their command's name is in the index.
/// shortcut: a fixed table, update it when the registry adds such an agent.
const LOCAL_COMMANDS: [(&str, &[&str]); 7] = [
    ("grok-build", &["grok", "agent", "stdio"]),
    ("gemini", &["gemini", "--acp"]),
    ("github-copilot-cli", &["copilot", "--acp"]),
    ("qwen-code", &["qwen", "--acp"]),
    ("factory-droid", &["droid", "exec", "--output-format", "acp-daemon"]),
    ("cline", &["cline", "--acp"]),
    ("auggie", &["auggie", "--acp"]),
];

/// This machine as the index names platforms.
pub fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    format!("{os}-{}", std::env::consts::ARCH)
}

fn command(program: PathBuf, args: &[String], env: &BTreeMap<String, String>) -> AgentCommand {
    AgentCommand {
        program,
        args: args.to_vec(),
        env: env.iter().map(|(key, value)| (key.clone(), value.clone())).collect(),
    }
}

impl RegistryAgent {
    /// The best way to run this agent here: the user's own install, then a
    /// package runner, then the vendor's binary. `find` looks a program up
    /// on the `PATH`; binaries go under `installs/<id>/<version>`.
    pub fn launch(&self, find: &dyn Fn(&str) -> Option<PathBuf>, installs: &Path) -> Option<Launch> {
        let binary = self.distribution.binary.get(&platform());
        let local = LOCAL_COMMANDS
            .iter()
            .find(|(id, _)| *id == self.id)
            .map(|(_, words)| words.iter().map(|word| (*word).to_owned()).collect::<Vec<_>>())
            .or_else(|| {
                let target = binary?;
                let name = Path::new(&target.cmd).file_name()?.to_str()?.to_owned();
                Some(std::iter::once(name).chain(target.args.iter().cloned()).collect())
            });
        if let Some(words) = local {
            if let Some(program) = find(&words[0]) {
                return Some(Launch::Local(command(program, &words[1..], &BTreeMap::new())));
            }
        }
        // The index comes from the network: its names become folders only
        // when they cannot climb out of `installs`.
        let binary = binary.filter(|_| plain_name(&self.id) && plain_name(&self.version));
        let dir = installs.join(&self.id).join(&self.version);
        if let Some(target) = binary {
            let installed = dir.join(&target.cmd);
            if installed.is_file() {
                return Some(Launch::Local(command(installed, &target.args, &target.env)));
            }
        }
        if let (Some(package), Some(npx)) = (&self.distribution.npx, find("npx")) {
            let args: Vec<String> = ["-y".to_owned(), package.package.clone()].into_iter().chain(package.args.iter().cloned()).collect();
            return Some(Launch::Package(command(npx, &args, &package.env)));
        }
        if let (Some(package), Some(uvx)) = (&self.distribution.uvx, find("uvx")) {
            let args: Vec<String> = std::iter::once(package.package.clone()).chain(package.args.iter().cloned()).collect();
            return Some(Launch::Package(command(uvx, &args, &package.env)));
        }
        binary.map(|target| Launch::Binary { target: target.clone(), dir })
    }
}

/// A name that is one path component, safe to join under a folder.
pub fn plain_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\'])
}

/// The program `name` on this process's `PATH`, as a shell would find it.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    find_in(&std::env::var_os("PATH")?, name)
}

/// The program `name` on `search_path`, a `PATH`-style list of folders.
pub fn find_in(search_path: &std::ffi::OsStr, name: &str) -> Option<PathBuf> {
    std::env::split_paths(search_path).map(|dir| dir.join(name)).find(|candidate| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            candidate.metadata().is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        }
        #[cfg(not(unix))]
        {
            candidate.is_file()
        }
    })
}

/// The index, from `cache_dir` when it is less than a day old, otherwise
/// from the network; a stale copy still answers when the network does not.
pub fn load(cache_dir: &Path) -> Result<Registry, String> {
    let cache = cache_dir.join(CACHE_FILE);
    let fresh = std::fs::metadata(&cache)
        .and_then(|meta| meta.modified())
        .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age < CACHE_AGE));
    if fresh {
        if let Some(registry) = std::fs::read(&cache).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()) {
            return Ok(registry);
        }
    }
    match download(REGISTRY_URL) {
        Ok(bytes) => {
            let registry: Registry =
                serde_json::from_slice(&bytes).map_err(|error| format!("El registro de agentes no es válido: {error}"))?;
            let _ = std::fs::create_dir_all(cache_dir).and_then(|()| std::fs::write(&cache, &bytes));
            Ok(registry)
        }
        Err(error) => std::fs::read(&cache)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or(error),
    }
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| format!("No se pudo descargar {url}: {error}"))?;
    response.bytes().map(|bytes| bytes.to_vec()).map_err(|error| format!("No se pudo descargar {url}: {error}"))
}

/// Downloads `target` into `dir` and unpacks it, checking its SHA-256 when
/// the index gives one. Returns how to run what was installed.
pub fn install(target: &BinaryTarget, dir: &Path) -> Result<AgentCommand, String> {
    install_with(target, dir, &download)
}

fn install_with(target: &BinaryTarget, dir: &Path, fetch: &dyn Fn(&str) -> Result<Vec<u8>, String>) -> Result<AgentCommand, String> {
    let bytes = fetch(&target.archive)?;
    if let Some(expected) = &target.sha256 {
        let actual = format!("{:x}", Sha256::digest(&bytes));
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(format!("La descarga de {} no coincide con su huella publicada", target.archive));
        }
    }
    // Unpacked beside the final place and moved in whole: a failed install
    // never leaves half an agent where `launch` would find it.
    let staging = dir.with_file_name(format!("{}.partial", dir.file_name().and_then(|name| name.to_str()).unwrap_or("agent")));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|error| error.to_string())?;
    let name = target.archive.rsplit('/').next().unwrap_or("download");
    let archive = staging.join(name);
    std::fs::write(&archive, &bytes).map_err(|error| error.to_string())?;
    let unpack = |program: &str, args: &[&str]| -> Result<(), String> {
        let status = std::process::Command::new(program)
            .args(args)
            .current_dir(&staging)
            .status()
            .map_err(|error| format!("No se pudo ejecutar {program} para descomprimir el agente: {error}"))?;
        status.success().then_some(()).ok_or_else(|| format!("{program} no pudo descomprimir {name}"))
    };
    // shortcut: unpacks with the system's `unzip` and `tar`; move to the zip
    // and tar crates if a platform without them is supported.
    if name.ends_with(".zip") {
        unpack("unzip", &["-q", "-o", name])?;
        std::fs::remove_file(&archive).map_err(|error| error.to_string())?;
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") || name.ends_with(".tar.bz2") || name.ends_with(".tar.xz") {
        unpack("tar", &["-xf", name])?;
        std::fs::remove_file(&archive).map_err(|error| error.to_string())?;
    } else {
        // A bare executable: it is the command itself.
        std::fs::rename(&archive, staging.join(&target.cmd)).map_err(|error| error.to_string())?;
    }
    let program = staging.join(&target.cmd);
    if !program.is_file() {
        return Err(format!("El paquete del agente no trae {}", target.cmd));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&program).map_err(|error| error.to_string())?.permissions();
        permissions.set_mode(permissions.mode() | 0o755);
        std::fs::set_permissions(&program, permissions).map_err(|error| error.to_string())?;
    }
    let _ = std::fs::remove_dir_all(dir);
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::rename(&staging, dir).map_err(|error| error.to_string())?;
    Ok(command(dir.join(&target.cmd), &target.args, &target.env))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Registry {
        serde_json::from_str(include_str!("../testdata/registry.json")).expect("the published index parses")
    }

    fn agent(id: &str) -> RegistryAgent {
        registry().agents.into_iter().find(|agent| agent.id == id).unwrap_or_else(|| panic!("{id} in the index"))
    }

    #[test]
    fn the_published_index_parses_with_its_distributions() {
        let registry = registry();
        assert!(registry.agents.len() >= 40);
        assert_eq!(agent("grok-build").distribution.npx.unwrap().args, ["agent", "stdio"]);
        assert!(agent("antigravity-acp").distribution.binary.contains_key("linux-x86_64"));
        assert!(agent("fast-agent").distribution.uvx.is_some());
    }

    #[test]
    fn an_installed_cli_goes_first_then_a_package_runner_then_the_binary() {
        let installs = Path::new("/nowhere");
        let none = |_: &str| None;
        let only = |name: &'static str| move |wanted: &str| (wanted == name).then(|| PathBuf::from(format!("/bin/{name}")));
        let Some(Launch::Local(grok)) = agent("grok-build").launch(&only("grok"), installs) else {
            panic!("the user's grok");
        };
        assert_eq!((grok.program.as_path(), grok.args.as_slice()), (Path::new("/bin/grok"), ["agent".to_owned(), "stdio".to_owned()].as_slice()));
        let Some(Launch::Package(npx)) = agent("gemini").launch(&only("npx"), installs) else {
            panic!("gemini through npx");
        };
        assert_eq!(npx.args[..2], ["-y".to_owned(), agent("gemini").distribution.npx.unwrap().package]);
        assert_eq!(npx.args.last().map(String::as_str), Some("--acp"));
        if platform() == "linux-x86_64" {
            let Some(Launch::Local(opencode)) = agent("opencode").launch(&only("opencode"), installs) else {
                panic!("opencode's own binary, named in the index");
            };
            assert_eq!(opencode.args, ["acp"]);
            let Some(Launch::Binary { dir, .. }) = agent("antigravity-acp").launch(&none, installs) else {
                panic!("Google's ACP server to download");
            };
            assert_eq!(dir, installs.join("antigravity-acp").join(agent("antigravity-acp").version));
        }
        assert_eq!(agent("fast-agent").launch(&none, installs), None);
        let climbing = RegistryAgent { id: "../../etc".into(), ..agent("antigravity-acp") };
        assert!(!matches!(climbing.launch(&none, installs), Some(Launch::Binary { .. })));
        assert!(plain_name("antigravity-acp") && plain_name("1.2.3") && !plain_name("..") && !plain_name("a/b"));
    }

    #[test]
    fn a_download_is_checked_unpacked_and_moved_in_whole() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        std::fs::create_dir_all(source.join("bin")).unwrap();
        std::fs::write(source.join("bin/agent"), "#!/bin/sh\necho hi\n").unwrap();
        let archive = root.path().join("agent.tar.gz");
        let packed = std::process::Command::new("tar")
            .args(["-czf", archive.to_str().unwrap(), "-C", source.to_str().unwrap(), "bin"])
            .status()
            .unwrap();
        assert!(packed.success());
        let bytes = std::fs::read(&archive).unwrap();
        let target = BinaryTarget {
            archive: "https://example.com/agent.tar.gz".into(),
            cmd: "./bin/agent".into(),
            args: vec!["acp".into()],
            env: BTreeMap::new(),
            sha256: Some(format!("{:x}", Sha256::digest(&bytes))),
        };
        let fetch = |_: &str| Ok(bytes.clone());
        let dir = root.path().join("installs/agent/1.0.0");
        let command = install_with(&target, &dir, &fetch).unwrap();
        assert_eq!(command.program, dir.join("./bin/agent"));
        assert!(command.program.is_file());
        assert!(!root.path().join("installs/agent/1.0.0.partial").exists());
        let tampered = BinaryTarget { sha256: Some("0".repeat(64)), ..target };
        let error = install_with(&tampered, &root.path().join("installs/agent/2.0.0"), &fetch).unwrap_err();
        assert!(error.contains("huella"), "{error}");
        assert!(!root.path().join("installs/agent/2.0.0").exists());
    }
}
