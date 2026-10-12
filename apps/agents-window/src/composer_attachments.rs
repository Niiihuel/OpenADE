//! Local attachment imports and bounded thumbnails, processed off the UI thread.

use std::{
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
};

use image::{ImageFormat, ImageReader};
use openide_core::{UserImage, UserImageMime, MAX_USER_IMAGE_BYTES};
use openide_ui::{Icon, RasterImage, RasterImageFormat};

const MAX_IMPORTED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DECODE_BYTES: u64 = 64 * 1024 * 1024;

pub(crate) type DraftKey = (String, Option<String>);

#[derive(Clone, Debug)]
pub(crate) struct ImageGalleryView {
    pub workspace: String,
    pub attachments: Vec<crate::view::ComposerAttachmentView>,
    pub selected: usize,
    pub generation: u64,
    pub image: Option<RasterImage>,
    pub error: Option<String>,
    pub loading: bool,
    /// How many times its fit to the window the picture is shown.
    pub zoom: f32,
    /// Confirmed journal images already held by the transcript. They never
    /// enter the attachment worker or acquire a machine-local file path.
    pub tool_source: Option<ToolImageGallery>,
}

#[derive(Clone, Debug)]
pub(crate) struct ToolImageGallery {
    pub owner: String,
    pub call_id: String,
    pub images: Vec<RasterImage>,
}

impl ImageGalleryView {
    pub fn image_count(&self) -> usize {
        self.tool_source
            .as_ref()
            .map_or(self.attachments.len(), |source| source.images.len())
    }

    pub fn is_current(&self, state: &crate::view::ViewState) -> bool {
        let Some(source) = &self.tool_source else {
            return true;
        };
        if state.selected_session.as_deref() != Some(source.owner.as_str())
            || self.workspace != state.workspace
        {
            return false;
        }
        let media = state.transcript.iter().enumerate().find_map(|(index, item)| {
            matches!(item, crate::transcript::TranscriptItem::Tool { id, .. } if id == &source.call_id)
                .then(|| crate::session_surface::tool_media(state, index, &source.call_id))
                .flatten()
        });
        let Some(crate::transcript::TranscriptItem::ToolMedia { images, .. }) = media else {
            return false;
        };
        let current = images.iter().filter_map(|image| image.raster.as_ref());
        current
            .map(RasterImage::sha256)
            .eq(source.images.iter().map(RasterImage::sha256))
            && self.image.as_ref().is_some_and(|image| {
                source
                    .images
                    .get(self.selected)
                    .is_some_and(|selected| selected.sha256() == image.sha256())
            })
    }
}

fn viewer_image(workspace: &Path, relative: &str) -> Result<RasterImage, String> {
    let root = workspace
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let path = workspace
        .join(relative)
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if !path.starts_with(&root) {
        return Err("La imagen está fuera del proyecto".into());
    }
    let file = File::open(&path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("La imagen no está disponible".into());
    }
    if metadata.len() > MAX_USER_IMAGE_BYTES as u64 {
        return Err("La imagen supera 8 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_USER_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_USER_IMAGE_BYTES {
        return Err("La imagen supera 8 MiB".into());
    }
    let guessed_format = image::guess_format(&bytes).map_err(|error| error.to_string())?;
    let (mime, format) = match guessed_format {
        ImageFormat::Png => (UserImageMime::Png, RasterImageFormat::Png),
        ImageFormat::Jpeg => (UserImageMime::Jpeg, RasterImageFormat::Jpeg),
        ImageFormat::WebP => (UserImageMime::Webp, RasterImageFormat::Webp),
        ImageFormat::Gif => (UserImageMime::Gif, RasterImageFormat::Gif),
        _ => return Err("Formato de imagen no disponible".into()),
    };
    // Keep the renderer's original-source limits for the full image as well.
    let mut reader = ImageReader::with_format(Cursor::new(&bytes), guessed_format);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    reader.limits(limits);
    let (width, height) = reader
        .into_dimensions()
        .map_err(|error| error.to_string())?;
    if width == 0
        || height == 0
        || width > 4096
        || height > 4096
        || u64::from(width) * u64::from(height) > 12 * 1024 * 1024
    {
        return Err(
            "La imagen supera el límite de vista completa (4096 px por lado y 12 megapíxeles)"
                .into(),
        );
    }
    let image = UserImage::from_bytes(mime, &bytes).map_err(|error| error.to_string())?;
    RasterImage::new(
        image.sha256,
        format,
        image.size_bytes,
        Arc::from(image.data_base64),
        "Imagen adjunta en vista completa",
    )
    .ok_or("La imagen no está disponible".into())
}

#[derive(Clone, Debug)]
pub(crate) struct AttachmentVisual {
    pub icon: Icon,
    pub is_image: bool,
    pub description: String,
    pub preview: Option<RasterImage>,
}

pub(crate) fn visual_key(workspace: &str, path: &str) -> String {
    format!("{}:{workspace}{path}", workspace.len())
}

pub(crate) fn fallback_visual(path: &str, is_dir: bool) -> AttachmentVisual {
    let extension = Path::new(path)
        .extension()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let is_image = !is_dir && matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif");
    let icon = if is_dir {
        openide_ui::folder_icon(path, false)
    } else {
        openide_ui::file_icon(path)
    };
    AttachmentVisual {
        icon,
        is_image,
        description: if is_dir {
            "Carpeta".into()
        } else if extension.is_empty() {
            "Archivo".into()
        } else {
            extension.to_uppercase()
        },
        preview: None,
    }
}

fn byte_label(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{} KB", bytes.div_ceil(1024))
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn inspect(workspace: &Path, relative: &str, is_dir: bool) -> AttachmentVisual {
    let mut visual = fallback_visual(relative, is_dir);
    let result = (|| -> Result<(), String> {
        let root = workspace
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let path = workspace
            .join(relative)
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if !path.starts_with(&root) {
            return Err("Ruta fuera del proyecto".into());
        }
        let metadata = fs::metadata(&path).map_err(|error| error.to_string())?;
        if metadata.is_dir() {
            return Ok(());
        }
        if !metadata.is_file() {
            return Err("Archivo no disponible".into());
        }
        visual
            .description
            .push_str(&format!(" · {}", byte_label(metadata.len())));
        if !visual.is_image {
            return Ok(());
        }
        if metadata.len() > MAX_USER_IMAGE_BYTES as u64 {
            return Err("Imagen mayor a 8 MiB".into());
        }
        let mut bytes = Vec::new();
        File::open(&path)
            .map_err(|error| error.to_string())?
            .take(MAX_USER_IMAGE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > MAX_USER_IMAGE_BYTES {
            return Err("Imagen mayor a 8 MiB".into());
        }
        let mut reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|error| error.to_string())?;
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(MAX_DECODE_BYTES);
        limits.max_image_width = Some(16_384);
        limits.max_image_height = Some(16_384);
        reader.limits(limits);
        let decoded = reader.decode().map_err(|error| error.to_string())?;
        visual
            .description
            .push_str(&format!(" · {} × {}", decoded.width(), decoded.height()));
        let mut encoded = Cursor::new(Vec::new());
        decoded
            .thumbnail(256, 256)
            .write_to(&mut encoded, ImageFormat::Png)
            .map_err(|error| error.to_string())?;
        let thumbnail = UserImage::from_bytes(UserImageMime::Png, &encoded.into_inner())
            .map_err(|error| error.to_string())?;
        visual.preview = RasterImage::new(
            thumbnail.sha256,
            RasterImageFormat::Png,
            thumbnail.size_bytes,
            Arc::from(thumbnail.data_base64),
            "Vista previa del adjunto",
        );
        Ok(())
    })();
    if result.is_err() {
        visual.description.push_str(if visual.is_image {
            " · Vista previa no disponible"
        } else {
            " · Dato no disponible"
        });
    }
    visual
}

fn file_urls(text: &str) -> Option<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for (index, line) in text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .enumerate()
    {
        if index == 0 && matches!(line, "copy" | "cut") {
            continue;
        }
        let url = reqwest::Url::parse(line).ok()?;
        if url.scheme() != "file" {
            return None;
        }
        paths.push(url.to_file_path().ok()?);
    }
    (!paths.is_empty()).then_some(paths)
}

pub(crate) fn is_file_paste(text: &str) -> bool {
    file_urls(text).is_some()
}

fn import_directory(root: &Path) -> Result<PathBuf, String> {
    let mut directory = root.to_owned();
    for name in [".openide", "attachments"] {
        directory.push(name);
        match fs::create_dir(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("No se pudo preparar el adjunto: {error}")),
        }
        directory = directory
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if !directory.starts_with(root) {
            return Err("La carpeta de adjuntos está fuera del proyecto".into());
        }
    }
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(".gitignore"))
    {
        Ok(mut file) => file.write_all(b"*\n").map_err(|error| error.to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.to_string()),
    }
    let directory = directory.join(uuid::Uuid::new_v4().to_string());
    fs::create_dir(&directory).map_err(|error| error.to_string())?;
    Ok(directory)
}

fn remove_import(path: &Path) {
    let _ = fs::remove_file(path);
    if let Some(directory) = path.parent() {
        let _ = fs::remove_dir(directory);
    }
}

fn import_file(root: &Path, source: &Path) -> Result<(PathBuf, bool), String> {
    let source = source
        .canonicalize()
        .map_err(|error| format!("No se pudo leer el archivo pegado: {error}"))?;
    if source.starts_with(root) {
        let metadata = fs::metadata(&source).map_err(|error| error.to_string())?;
        if !metadata.is_file() && !metadata.is_dir() {
            return Err("El adjunto debe ser un archivo o carpeta".into());
        }
        return Ok((source, false));
    }
    let file = File::open(&source).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("Las carpetas pegadas deben pertenecer al proyecto".into());
    }
    if metadata.len() > MAX_IMPORTED_BYTES {
        return Err("El archivo pegado supera 64 MiB".into());
    }
    let directory = import_directory(root)?;
    let destination = directory.join(source.file_name().ok_or("El archivo no tiene nombre")?);
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
        .map_err(|error| error.to_string())?;
    let copied = std::io::copy(&mut file.take(MAX_IMPORTED_BYTES + 1), &mut output);
    drop(output);
    let copied = copied.map_err(|error| {
        remove_import(&destination);
        error.to_string()
    })?;
    if copied > MAX_IMPORTED_BYTES {
        remove_import(&destination);
        return Err("El archivo pegado supera 64 MiB".into());
    }
    Ok((destination, true))
}

fn paste(workspace: &Path, hint: Option<&str>, available: usize) -> Result<Vec<PathBuf>, String> {
    let root = workspace
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let hinted_paths = hint.and_then(file_urls);
    let mut clipboard = if hinted_paths.is_none() {
        Some(
            arboard::Clipboard::new()
                .map_err(|error| format!("Portapapeles no disponible: {error}"))?,
        )
    } else {
        None
    };
    let paths = hinted_paths.or_else(|| clipboard.as_mut()?.get().file_list().ok());
    if let Some(paths) = paths.filter(|paths| !paths.is_empty()) {
        if paths.len() > available {
            return Err(format!("Sólo hay espacio para {available} adjuntos más"));
        }
        let mut imported = Vec::new();
        for path in paths {
            match import_file(&root, &path) {
                Ok(file) => imported.push(file),
                Err(error) => {
                    for (path, copied) in imported {
                        if copied {
                            remove_import(&path);
                        }
                    }
                    return Err(error);
                }
            }
        }
        return Ok(imported.into_iter().map(|(path, _)| path).collect());
    }
    if available == 0 {
        return Err("Máximo de 12 adjuntos".into());
    }
    let image = clipboard
        .as_mut()
        .ok_or("Portapapeles no disponible")?
        .get_image()
        .map_err(|_| "No hay imágenes ni archivos en el portapapeles".to_owned())?;
    let size = image
        .width
        .checked_mul(image.height)
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|size| *size <= MAX_DECODE_BYTES as usize)
        .ok_or("La imagen del portapapeles es demasiado grande")?;
    if size != image.bytes.len() {
        return Err("Imagen del portapapeles no válida".into());
    }
    let pixels = image::RgbaImage::from_raw(
        image
            .width
            .try_into()
            .map_err(|_| "Imagen demasiado ancha")?,
        image
            .height
            .try_into()
            .map_err(|_| "Imagen demasiado alta")?,
        image.bytes.into_owned(),
    )
    .ok_or("Imagen del portapapeles no válida")?;
    let mut encoded = Cursor::new(Vec::new());
    pixels
        .write_to(&mut encoded, ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    let bytes = encoded.into_inner();
    if bytes.len() > MAX_USER_IMAGE_BYTES {
        return Err("La imagen pegada supera 8 MiB".into());
    }
    let directory = import_directory(&root)?;
    let destination = directory.join("imagen.png");
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
        .map_err(|error| error.to_string())?;
    let written = output.write_all(&bytes);
    drop(output);
    written.map_err(|error| {
        remove_import(&destination);
        error.to_string()
    })?;
    Ok(vec![destination])
}

enum Request {
    Image {
        workspace: String,
        path: String,
        generation: u64,
    },
    Inspect {
        workspace: String,
        path: String,
        is_dir: bool,
    },
    Paste {
        key: DraftKey,
        hint: Option<String>,
        available: usize,
    },
}

pub(crate) enum Response {
    Image {
        generation: u64,
        result: Result<RasterImage, String>,
    },
    Visual {
        key: String,
        visual: AttachmentVisual,
    },
    Pasted {
        key: DraftKey,
        result: Result<Vec<PathBuf>, String>,
    },
}

pub(crate) struct AttachmentWorker {
    requests: SyncSender<Request>,
    responses: Receiver<Response>,
}

impl AttachmentWorker {
    pub fn start() -> Option<Self> {
        let (requests, receiver) = mpsc::sync_channel(16);
        let (sender, responses) = mpsc::sync_channel(32);
        std::thread::Builder::new()
            .name("composer-attachments".into())
            .spawn(move || {
                while let Ok(request) = receiver.recv() {
                    let response = match request {
                        Request::Image {
                            workspace,
                            path,
                            generation,
                        } => Response::Image {
                            generation,
                            result: viewer_image(Path::new(&workspace), &path),
                        },
                        Request::Inspect {
                            workspace,
                            path,
                            is_dir,
                        } => Response::Visual {
                            key: visual_key(&workspace, &path),
                            visual: inspect(Path::new(&workspace), &path, is_dir),
                        },
                        Request::Paste {
                            key,
                            hint,
                            available,
                        } => Response::Pasted {
                            result: paste(Path::new(&key.0), hint.as_deref(), available),
                            key,
                        },
                    };
                    if sender.send(response).is_err() {
                        break;
                    }
                }
            })
            .ok()?;
        Some(Self {
            requests,
            responses,
        })
    }

    pub fn inspect(&self, workspace: &str, path: &str, is_dir: bool) -> bool {
        self.requests
            .try_send(Request::Inspect {
                workspace: workspace.into(),
                path: path.into(),
                is_dir,
            })
            .is_ok()
    }

    pub fn paste(&self, key: DraftKey, hint: Option<String>, available: usize) -> bool {
        self.requests
            .try_send(Request::Paste {
                key,
                hint,
                available,
            })
            .is_ok()
    }

    pub fn poll(&self) -> Option<Response> {
        self.responses.try_recv().ok()
    }

    pub fn image(&self, workspace: &str, path: &str, generation: u64) -> bool {
        self.requests
            .try_send(Request::Image {
                workspace: workspace.into(),
                path: path.into(),
                generation,
            })
            .is_ok()
    }
}
