//! What the agent may do in the built-in browser on each site without being
//! asked. A site is an origin: scheme, host and port. One not listed here is
//! asked about, action by action, as before.

use serde::{Deserialize, Serialize};
use std::{io::Read, path::{Path, PathBuf}};

const FILE_NAME: &str = "browser-sites.json";
const VERSION: u32 = 1;
const MAX_SITES: usize = 256;
const MAX_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SiteAccess {
    /// The agent acts on the site without asking.
    Allow,
    /// Each action is asked about.
    #[default]
    Ask,
    /// The agent's actions on the site are refused without asking.
    Block,
}

impl SiteAccess {
    pub const ALL: [Self; 3] = [Self::Allow, Self::Ask, Self::Block];

    pub fn key(self) -> &'static str {
        match self { Self::Allow => "allow", Self::Ask => "ask", Self::Block => "block" }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|access| access.key() == key)
    }

}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Site {
    pub origin: String,
    pub access: SiteAccess,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BrowserSites {
    pub sites: Vec<Site>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    sites: Vec<Site>,
}

/// The origin of a web address: `https://example.com`, or with its port when
/// it is not the scheme's own. Nothing for an address that is not of the web.
pub(crate) fn origin_of(url: &str) -> Option<String> {
    let url = reqwest::Url::parse(url.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    })
}

/// The site a call of one of the browser's tools acts on: where it is told
/// to go, or the page it is on. Nothing for any other tool.
pub(crate) fn site_of_call(tool_name: &str, arguments: &str, page_url: &str) -> Option<String> {
    let tool = tool_name.strip_prefix(crate::browser_runtime::NATIVE_TOOL_PREFIX)?;
    let going = matches!(tool, "browser_navigate" | "browser_tab_new" | "open_preview")
        .then(|| serde_json::from_str::<serde_json::Value>(arguments).ok())
        .flatten()
        .and_then(|arguments| origin_of(arguments.get("url")?.as_str()?));
    going.or_else(|| origin_of(page_url))
}

impl BrowserSites {
    pub fn load(root: &Path) -> Result<Self, String> {
        let path = root.join(FILE_NAME);
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)] {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(format!("No se pudieron leer los permisos por sitio: {error}")),
        };
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err("Los permisos por sitio deben guardarse en un archivo regular".into());
        }
        if metadata.len() > MAX_BYTES {
            return Err("El archivo de permisos por sitio es demasiado grande".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)
            .map_err(|error| format!("No se pudieron leer los permisos por sitio: {error}"))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("El archivo de permisos por sitio es demasiado grande".into());
        }
        let document: Document =
            serde_json::from_slice(&bytes).map_err(|error| format!("Los permisos por sitio no son válidos: {error}"))?;
        if document.version != VERSION {
            return Err("Los permisos por sitio no son válidos".into());
        }
        let sites = Self { sites: document.sites };
        sites.encoded()?;
        Ok(sites)
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let bytes = self.encoded()?;
        openide_harness::atomic_write(&root.join(FILE_NAME), &bytes)
            .map_err(|error| format!("No se pudieron guardar los permisos por sitio: {error}"))
    }

    fn encoded(&self) -> Result<Vec<u8>, String> {
        let mut origins = std::collections::HashSet::new();
        if self.sites.len() > MAX_SITES || self.sites.iter().any(|site| {
            site.origin.len() > 4096
                || origin_of(&site.origin).as_deref() != Some(site.origin.as_str())
                || !origins.insert(site.origin.as_str())
        }) {
            return Err("Los permisos por sitio no son válidos".into());
        }
        let bytes = serde_json::to_vec_pretty(&Document { version: VERSION, sites: self.sites.clone() })
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("El archivo de permisos por sitio es demasiado grande".into());
        }
        Ok(bytes)
    }

    /// Explicit recovery only. Keep the complete original, including an
    /// oversized file, without reading it into memory or overwriting a backup.
    pub fn reset_with_backup(root: &Path) -> Result<(Self, PathBuf), String> {
        if Self::load(root).is_ok() {
            return Err("El archivo de permisos ya es válido. Recargalo para conservar sus reglas.".into());
        }
        let path = root.join(FILE_NAME);
        let backup = root.join(format!("browser-sites-backup-{}.json", uuid::Uuid::new_v4()));
        std::fs::rename(&path, &backup)
            .map_err(|error| format!("No se pudo conservar una copia de los permisos: {error}"))?;
        let sites = Self::default();
        if let Err(error) = sites.save(root) {
            return match std::fs::rename(&backup, &path) {
                Ok(()) => Err(error),
                Err(restore) => Err(format!("{error}. La copia se conserva en {}. No se pudo restaurar: {restore}", backup.display())),
            };
        }
        Ok((sites, backup))
    }

    pub fn access(&self, origin: &str) -> SiteAccess {
        self.sites.iter().find(|site| site.origin == origin).map(|site| site.access).unwrap_or_default()
    }

    /// Gives a site its own permission. `address` may be a whole address: its
    /// origin is what is kept.
    pub fn set(&mut self, address: &str, access: SiteAccess) -> Result<(), String> {
        let address = address.trim();
        if address.len() > 4096 {
            return Err("La dirección del sitio es demasiado larga".into());
        }
        // A host alone is its site over HTTPS.
        let origin = origin_of(address)
            .or_else(|| (!address.contains("://")).then(|| origin_of(&format!("https://{address}"))).flatten())
            .ok_or("Ingresá un sitio válido, como https://example.com")?;
        let mut next = self.clone();
        match next.sites.iter().position(|site| site.origin == origin) {
            Some(index) => next.sites[index].access = access,
            None if next.sites.len() >= MAX_SITES => return Err("Hay demasiados sitios con permisos propios".into()),
            None => next.sites.push(Site { origin, access }),
        }
        next.encoded()?;
        *self = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_site_is_the_origin_of_a_web_address_and_of_nothing_else() {
        assert_eq!(origin_of("https://Example.com/a/b?c#d").as_deref(), Some("https://example.com"));
        assert_eq!(origin_of("http://localhost:3000/panel").as_deref(), Some("http://localhost:3000"));
        assert_eq!(origin_of("https://example.com:443/").as_deref(), Some("https://example.com"));
        for other in ["about:blank", "file:///etc/passwd", "data:text/html,x", "javascript:1", "", "example.com"] {
            assert_eq!(origin_of(other), None, "{other}");
        }
    }

    #[test]
    fn a_call_acts_on_where_it_goes_or_on_the_page_it_is_on() {
        let tool = |name: &str| format!("{}{name}", crate::browser_runtime::NATIVE_TOOL_PREFIX);
        let page = "https://tienda.example/carrito";
        assert_eq!(site_of_call(&tool("browser_click"), r#"{"ref":"e3"}"#, page).as_deref(), Some("https://tienda.example"));
        assert_eq!(
            site_of_call(&tool("browser_navigate"), r#"{"url":"https://otro.example/x"}"#, page).as_deref(),
            Some("https://otro.example")
        );
        // A new blank tab is opened from the page it is asked on.
        assert_eq!(site_of_call(&tool("browser_tab_new"), "{}", page).as_deref(), Some("https://tienda.example"));
        // Not a tool of the browser, or no page of the web to act on.
        assert_eq!(site_of_call("run_command", r#"{"url":"https://otro.example"}"#, page), None);
        assert_eq!(site_of_call("mcp__other__browser_click", "{}", page), None);
        assert_eq!(site_of_call(&tool("browser_click"), "{}", "about:blank"), None);
    }

    #[test]
    fn permissions_are_kept_by_origin_and_survive_a_restart() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(BrowserSites::load(root.path()).unwrap(), BrowserSites::default());
        let mut sites = BrowserSites::default();
        sites.set("https://example.com/algo", SiteAccess::Allow).unwrap();
        sites.set("localhost:3000", SiteAccess::Block).unwrap();
        // The same site again changes its value instead of adding a row.
        sites.set("https://example.com", SiteAccess::Block).unwrap();
        sites.set("https://localhost:3000/", SiteAccess::Allow).unwrap();
        assert!(sites.set("no es un sitio", SiteAccess::Allow).is_err());
        assert!(sites.set("file:///etc", SiteAccess::Allow).is_err());
        assert_eq!(sites.sites.len(), 2);
        assert_eq!(sites.access("https://example.com"), SiteAccess::Block);
        assert_eq!(sites.access("https://localhost:3000"), SiteAccess::Allow);
        assert_eq!(sites.access("https://nunca.example"), SiteAccess::Ask);
        sites.save(root.path()).unwrap();
        assert_eq!(BrowserSites::load(root.path()).unwrap(), sites);
        // A file that names something other than an origin is refused whole.
        std::fs::write(root.path().join(FILE_NAME), r#"{"version":1,"sites":[{"origin":"https://example.com/ruta","access":"allow"}]}"#).unwrap();
        assert!(BrowserSites::load(root.path()).is_err());
    }

    #[test]
    fn invalid_and_oversized_site_policies_cannot_replace_a_valid_file() {
        let root = tempfile::tempdir().unwrap();
        let mut sites = BrowserSites::default();
        sites.set("https://blocked.example", SiteAccess::Block).unwrap();
        sites.save(root.path()).unwrap();
        let original = std::fs::read(root.path().join(FILE_NAME)).unwrap();
        let mut duplicate = sites.clone();
        duplicate.sites.push(duplicate.sites[0].clone());
        assert!(duplicate.save(root.path()).is_err());
        let mut invalid = sites.clone();
        invalid.sites[0].origin.push_str("/not-an-origin");
        assert!(invalid.save(root.path()).is_err());
        assert!(sites.set(&format!("https://{}.example", "x".repeat(MAX_BYTES as usize)), SiteAccess::Allow).is_err());
        assert_eq!(std::fs::read(root.path().join(FILE_NAME)).unwrap(), original);
        std::fs::write(root.path().join(FILE_NAME), vec![b' '; MAX_BYTES as usize + 1]).unwrap();
        assert!(BrowserSites::load(root.path()).is_err());
    }

    #[test]
    fn recovery_preserves_the_exact_invalid_file_and_never_resets_a_repaired_policy() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(FILE_NAME);
        let broken = b"{\"version\":1,\"sites\":[broken";
        std::fs::write(&path, broken).unwrap();
        let (recovered, backup) = BrowserSites::reset_with_backup(root.path()).unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), broken);
        assert_eq!(BrowserSites::load(root.path()).unwrap(), recovered);
        assert!(recovered.sites.is_empty());
        assert_ne!(backup, path);
        let mut repaired = BrowserSites::default();
        repaired.set("https://blocked.example", SiteAccess::Block).unwrap();
        repaired.save(root.path()).unwrap();
        assert!(BrowserSites::reset_with_backup(root.path()).is_err());
        assert_eq!(BrowserSites::load(root.path()).unwrap(), repaired);
        assert_eq!(std::fs::read(backup).unwrap(), broken);
    }

    #[cfg(unix)]
    #[test]
    fn a_named_pipe_is_rejected_without_waiting_for_a_writer() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let root = tempfile::tempdir().unwrap();
        let path = CString::new(root.path().join(FILE_NAME).as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let directory = root.path().to_owned();
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || { let _ = sender.send(BrowserSites::load(&directory)); });
        let result = receiver.recv_timeout(std::time::Duration::from_secs(1))
            .expect("a policy FIFO must not block the settings thread");
        assert!(result.is_err());
        worker.join().unwrap();
    }
}
