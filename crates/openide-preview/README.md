# openide-preview

Native Rust preview engine shared by the editor and agent workspace. The crate
returns a Markdown document model or a raster image to `openide-ui`; it does not
create a webview, run an external viewer, or access the network.

- Markdown: `pulldown-cmark` produces an inert document model. A live editor buffer
  takes precedence over disk, so an unsaved document can be previewed. Local
  images resolve relative to the document and remain inside the workspace.
- Images: `image` decodes PNG, JPEG, WebP and the first GIF frame. `resvg` renders
  SVG with system fonts and bounded embedded raster images. External SVG image
  references are disabled.
- PDF: `hayro = 0.7.1` rasterizes the requested page. The worker retains one parsed
  PDF, identified by canonical path, file size, modification time and, on Unix,
  device/inode. It does not render every page when a file opens.

File access rejects symbolic links, including links to other files in the
workspace. On Unix, directory descriptors and `openat` with `O_NOFOLLOW` prevent
concurrent path substitutions from redirecting reads. `O_NONBLOCK` prevents a
substituted FIFO from blocking before the regular-file check. On other platforms,
canonical path and link checks are best effort; a native directory-handle walk
would be needed to close ancestor substitution races there. File size is checked
both on the opened handle and during the bounded read.

One worker owns one pending request and one response. New work replaces queued
work. Responses retain workspace, path, tab identity and generation; the host
must compare the entire scope before applying them. Cancellation discards stale
results between operations; Hayro and resvg do not provide interruption during
an individual render. The documented limits constrain input and output, not all
internal allocations of third-party document interpreters.

Inputs are limited to 32 MiB per file and 2 MiB of Markdown. Raster source images
are limited to 4096 pixels per side and 16 megapixels. Rendered images are limited
to 4096 pixels per side, 4 megapixels and 8 MiB of PNG data. PDF requests render at
most 1600 pixels wide. Markdown retains at most 16 images and 32 MiB of PNG data.
Errors and unsupported assets are returned explicitly for the native UI.

Hayro is a developing PDF implementation; complex documents can expose rendering
limitations. Its standard-font and CMap features are included. Password requests,
PDF text selection, forms and annotations editing are not part of this API.

References: [Hayro](https://github.com/LaurenzV/hayro) (MIT OR Apache-2.0),
[resvg](https://github.com/linebender/resvg) (MIT OR Apache-2.0),
[image](https://github.com/image-rs/image) (MIT OR Apache-2.0), and
[pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) (MIT).
