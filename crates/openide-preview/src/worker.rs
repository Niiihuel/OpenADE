//! One lazy host-owned worker and one pending/result slot. Rapid tab, page and
//! zoom changes replace queued requests rather than building a render backlog.
use crate::{
    kind_for_path, markdown, media::Media, path, Document, PreviewKind, Request, Response,
};
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

const MAX_MARKDOWN_BYTES: usize = 2 * 1024 * 1024;
const MAX_MARKDOWN_IMAGES: usize = 16;
const MAX_MARKDOWN_IMAGE_BYTES: usize = 32 * 1024 * 1024;

struct Pending {
    serial: u64,
    request: Request,
}

#[derive(Default)]
struct Shared {
    request: Mutex<Option<Pending>>,
    result: Mutex<Option<(u64, Response)>>,
    wake: Condvar,
    serial: AtomicU64,
    stopping: AtomicBool,
}

pub struct Worker {
    shared: Arc<Shared>,
}

impl Worker {
    pub fn new() -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("openide-native-preview".into())
            .spawn(move || run(worker))?;
        Ok(Self { shared })
    }

    pub fn submit(&self, request: Request) -> Result<(), String> {
        if request
            .source
            .as_ref()
            .is_some_and(|source| source.len() > MAX_MARKDOWN_BYTES)
        {
            return Err("El Markdown supera el límite de vista previa de 2 MiB.".into());
        }
        let serial = self
            .shared
            .serial
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let mut pending = self
            .shared
            .request
            .lock()
            .map_err(|_| "El motor de vista previa no está disponible.".to_owned())?;
        *pending = Some(Pending { serial, request });
        self.shared.wake.notify_one();
        Ok(())
    }

    /// Discard both queued and completed work. An ongoing third-party PDF/SVG
    /// rasterization finishes on this one worker; its result is then discarded.
    pub fn cancel(&self) {
        self.shared.serial.fetch_add(1, Ordering::AcqRel);
        if let Ok(mut pending) = self.shared.request.lock() {
            *pending = None;
        }
        if let Ok(mut result) = self.shared.result.lock() {
            *result = None;
        }
    }

    pub fn try_recv(&self) -> Option<Response> {
        let (serial, response) = self.shared.result.lock().ok()?.take()?;
        (serial == self.shared.serial.load(Ordering::Acquire)).then_some(response)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        if let Ok(_pending) = self.shared.request.lock() {
            self.shared.wake.notify_one();
        }
    }
}

fn run(shared: Arc<Shared>) {
    // Hayro's caches are not Send. They are created and retained only here.
    let mut media = Media::default();
    loop {
        let next = {
            let Ok(mut pending) = shared.request.lock() else {
                return;
            };
            while pending.is_none() && !shared.stopping.load(Ordering::Acquire) {
                let Ok(ready) = shared.wake.wait(pending) else {
                    return;
                };
                pending = ready;
            }
            if shared.stopping.load(Ordering::Acquire) {
                return;
            }
            pending.take().unwrap()
        };
        let cancelled = || {
            shared.stopping.load(Ordering::Acquire)
                || shared.serial.load(Ordering::Acquire) != next.serial
        };
        if cancelled() {
            continue;
        }
        let result = catch_unwind(AssertUnwindSafe(|| {
            execute(&next.request, &mut media, &cancelled)
        }));
        let result = match result {
            Ok(result) => result,
            Err(_) => {
                media = Media::default();
                Err("No se pudo interpretar este documento.".into())
            }
        };
        if cancelled() {
            continue;
        }
        let response = Response {
            scope: next.request.scope,
            result,
        };
        let Ok(mut ready) = shared.result.lock() else {
            return;
        };
        *ready = Some((next.serial, response));
    }
}

fn execute(
    request: &Request,
    media: &mut Media,
    cancelled: &impl Fn() -> bool,
) -> Result<Document, String> {
    path::validate_relative(&request.scope.path)?;
    let kind =
        kind_for_path(&request.scope.path).ok_or("Este tipo de archivo no tiene vista previa.")?;
    match kind {
        PreviewKind::Pdf => media.pdf(request),
        PreviewKind::Image => media
            .image(
                &request.scope.workspace,
                &request.scope.path,
                request.render_width,
            )
            .map(Document::Image),
        PreviewKind::Markdown => {
            let disk_source;
            let source = if let Some(source) = &request.source {
                source.as_str()
            } else {
                let file = path::open(&request.scope.workspace, &request.scope.path)?;
                if file.identity.bytes as usize > MAX_MARKDOWN_BYTES {
                    return Err("El Markdown supera el límite de vista previa de 2 MiB.".into());
                }
                disk_source = String::from_utf8(file.read()?)
                    .map_err(|_| "El documento Markdown no es texto UTF-8.".to_owned())?;
                disk_source.as_str()
            };
            if source.len() > MAX_MARKDOWN_BYTES {
                return Err("El Markdown supera el límite de vista previa de 2 MiB.".into());
            }
            let mut document = markdown::parse(source);
            let mut image_bytes = 0_usize;
            for (index, image) in document.images.iter_mut().enumerate() {
                if cancelled() {
                    return Err("Vista previa reemplazada por otra solicitud.".into());
                }
                if index >= MAX_MARKDOWN_IMAGES {
                    image.error = Some("Se alcanzó el límite de 16 imágenes por documento.".into());
                    continue;
                }
                let loaded = path::markdown_asset(&request.scope.path, &image.destination)
                    .and_then(|path| {
                        media.image(&request.scope.workspace, &path, request.render_width)
                    });
                match loaded {
                    Ok(raster)
                        if image_bytes.saturating_add(raster.png.len())
                            <= MAX_MARKDOWN_IMAGE_BYTES =>
                    {
                        image_bytes += raster.png.len();
                        image.image = Some(raster);
                    }
                    Ok(_) => {
                        image.error = Some(
                            "Las imágenes superan el límite de memoria de vista previa.".into(),
                        )
                    }
                    Err(error) => image.error = Some(error),
                }
            }
            Ok(Document::Markdown(document))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Scope;
    use std::path::Path;
    use std::time::{Duration, Instant};

    fn request(root: &Path, path: &str, generation: u64) -> Request {
        Request {
            scope: Scope {
                workspace: root.into(),
                path: path.into(),
                tab_id: 1,
                generation,
            },
            source: None,
            page: 0,
            render_width: 200,
        }
    }

    fn receive(worker: &Worker) -> Response {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(response) = worker.try_recv() {
                return response;
            }
            assert!(Instant::now() < deadline, "preview worker timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn preview_worker_renders_pdf_and_discards_superseded_requests() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("file.pdf"), crate::media::test_pdf()).unwrap();
        let worker = Worker::new().unwrap();
        worker
            .submit(request(root.path(), "missing.pdf", 1))
            .unwrap();
        worker.submit(request(root.path(), "file.pdf", 2)).unwrap();
        let response = receive(&worker);
        assert_eq!(response.scope.generation, 2);
        assert!(matches!(
            response.result,
            Ok(Document::Pdf {
                pages: 2,
                page: 0,
                ..
            })
        ));
        worker.submit(request(root.path(), "file.pdf", 3)).unwrap();
        worker.cancel();
        assert!(worker.try_recv().is_none());
    }

    #[test]
    fn preview_worker_uses_unsaved_markdown_and_loads_only_local_scoped_images() {
        let root = tempfile::tempdir().unwrap();
        image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]))
            .save(root.path().join("local.png"))
            .unwrap();
        std::fs::write(root.path().join("file.md"), "old text").unwrap();
        let worker = Worker::new().unwrap();
        let mut req = request(root.path(), "file.md", 1);
        req.source =
            Some("# Unsaved\n\n![local](local.png)\n\n![remote](https://example.com/a.png)".into());
        worker.submit(req).unwrap();
        let response = receive(&worker);
        let Document::Markdown(document) = response.result.unwrap() else {
            panic!("expected Markdown")
        };
        assert_eq!(document.images.len(), 2);
        assert!(document.images[0].image.is_some());
        assert!(document.images[0].error.is_none());
        assert!(document.images[1].image.is_none());
        assert!(document.images[1].error.is_some());
        assert_eq!(
            std::fs::read_to_string(root.path().join("file.md")).unwrap(),
            "old text"
        );
    }

    #[test]
    fn preview_worker_preserves_scope_on_errors_and_bounds_live_source() {
        let root = tempfile::tempdir().unwrap();
        let worker = Worker::new().unwrap();
        let request = request(root.path(), "../outside.pdf", 42);
        let expected = request.scope.clone();
        worker.submit(request).unwrap();
        let response = receive(&worker);
        assert_eq!(response.scope, expected);
        assert!(response.result.is_err());
        let mut oversized = super::tests::request(root.path(), "file.md", 43);
        oversized.source = Some("x".repeat(MAX_MARKDOWN_BYTES + 1));
        assert!(worker.submit(oversized).is_err());
    }
}
