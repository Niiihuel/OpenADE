//! The pointer the agent is seen to act with on a page of the built-in
//! browser. The window paints it over the page's surface: the page is sent
//! none of it, so no script reads it and no screenshot shows it.
//!
//! Its picture is the user's own cursor theme, Bibata Modern Classic when it
//! is installed: an arrow that glides to where the agent acts, a ring where
//! it clicks and the text cursor while it writes.
// shortcut: painted with egui over one surface, move it into limn when a second surface needs a guided pointer.

use eframe::egui;
use openide_browser::Cue;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Where the agent's pointer goes on the page and what it does there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AgentCue {
    pub kind: Cue,
    /// Where it was, in CSS pixels of the viewport; `None` the first time.
    pub from: Option<(f32, f32)>,
    pub to: (f32, f32),
    pub started: Instant,
    /// How long it takes to get there: the action waits that long when the
    /// page is watched, so it lands as the pointer does.
    pub travel: Duration,
}

/// The ring and the press of a click, after the pointer arrived.
const PULSE: f32 = 0.42;
/// How long the pointer stays where it acted before it fades.
const REST: f32 = 2.4;
const FADE: f32 = 0.35;
const APPEAR: f32 = 0.14;

/// How the pointer looks at one moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pose {
    pub at: (f32, f32),
    pub alpha: f32,
    /// 0 at rest, 1 fully pressed.
    pub press: f32,
    /// The ring of a click: its radius in CSS pixels and its opacity.
    pub ring: Option<(f32, f32)>,
    /// The pointer got there: a field that is written in shows the text cursor.
    pub arrived: bool,
    /// Something still moves: the next frame differs from this one.
    pub moving: bool,
}

impl AgentCue {
    pub(crate) fn new(kind: Cue, from: Option<(f32, f32)>, to: (f32, f32), started: Instant) -> Self {
        // A first appearance comes in from nearby.
        let distance = from.map_or(240.0, |from| (to.0 - from.0).hypot(to.1 - from.1));
        // Already there: nothing to travel, and nothing for the action to wait for.
        let seconds = if distance < 1.0 { 0.0 } else { (0.12 + distance * 0.0005).min(0.45) };
        Self { kind, from, to, started, travel: Duration::from_secs_f32(seconds) }
    }

    /// The pointer at `now`. It starts from `origin` when it has no last
    /// place. `None` once it faded: there is nothing left to paint.
    pub(crate) fn pose(&self, now: Instant, origin: (f32, f32)) -> Option<Pose> {
        let elapsed = now.saturating_duration_since(self.started).as_secs_f32();
        let travel = self.travel.as_secs_f32();
        let since = elapsed - travel;
        let fade = 1.0 - ((since - REST) / FADE).clamp(0.0, 1.0);
        if fade <= 0.0 {
            return None;
        }
        let progress = if travel > 0.0 { (elapsed / travel).min(1.0) } else { 1.0 };
        // Leaves gently and settles gently, as a hand moves a pointer.
        let eased = if progress < 0.5 {
            4.0 * progress.powi(3)
        } else {
            1.0 - (-2.0 * progress + 2.0).powi(3) / 2.0
        };
        let from = self.from.unwrap_or(origin);
        let at = (from.0 + (self.to.0 - from.0) * eased, from.1 + (self.to.1 - from.1) * eased);
        let appear = if self.from.is_none() { (elapsed / APPEAR).min(1.0) } else { 1.0 };
        let pulse = (self.kind != Cue::Move && (0.0..PULSE).contains(&since)).then(|| since / PULSE);
        Some(Pose {
            at,
            alpha: appear * fade,
            // Down at once and back up in the first half of the pulse.
            press: pulse.map_or(0.0, |pulse| (std::f32::consts::PI * (pulse * 2.0).min(1.0)).sin()),
            ring: pulse.map(|pulse| (6.0 + 20.0 * (1.0 - (1.0 - pulse).powi(3)), 0.5 * (1.0 - pulse))),
            arrived: since >= 0.0,
            moving: since < 0.0 || pulse.is_some() || appear < 1.0 || since > REST,
        })
    }
}

/// One picture of a cursor theme: straight pixels as the theme stores them.
#[derive(Debug, PartialEq)]
struct CursorImage {
    size: [usize; 2],
    hot: (f32, f32),
    /// Premultiplied RGBA.
    rgba: Vec<u8>,
}

const XCURSOR_IMAGE: u32 = 0xfffd_0002;
/// No cursor is larger; a file that says otherwise is not read.
const MAX_SIDE: usize = 512;

/// Whether a picture of `nominal` pixels serves a pointer of `wanted` better
/// than one of `chosen`: the smallest that is not under it, else the largest.
fn fits_better(nominal: u32, chosen: u32, wanted: u32) -> bool {
    match (chosen >= wanted, nominal >= wanted) {
        (true, true) => nominal < chosen,
        (false, false) => nominal > chosen,
        (enough, _) => !enough,
    }
}

/// The picture of an Xcursor file whose nominal size is the smallest one
/// not under `wanted` pixels, or the largest there is.
fn parse_xcursor(file: &[u8], wanted: u32) -> Option<CursorImage> {
    let word = |at: usize| Some(u32::from_le_bytes(file.get(at..at + 4)?.try_into().ok()?));
    if file.get(..4)? != b"Xcur" {
        return None;
    }
    let (header, entries) = (word(4)? as usize, word(12)?.min(256) as usize);
    let at = (0..entries)
        .filter_map(|entry| {
            let entry = header + entry * 12;
            (word(entry)? == XCURSOR_IMAGE).then_some((word(entry + 4)?, word(entry + 8)? as usize))
        })
        // The first picture of each size: an animated cursor has several.
        .fold(None::<(u32, usize)>, |best, (nominal, at)| match best {
            Some((chosen, _)) if !fits_better(nominal, chosen, wanted) => best,
            _ => Some((nominal, at)),
        })?
        .1;
    let (width, height) = (word(at + 16)? as usize, word(at + 20)? as usize);
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return None;
    }
    let pixels = file.get(at + 36..at + 36 + width * height * 4)?;
    Some(CursorImage {
        size: [width, height],
        hot: (word(at + 24)? as f32, word(at + 28)? as f32),
        // Stored as little-endian ARGB: blue first.
        rgba: pixels.chunks_exact(4).flat_map(|bgra| [bgra[2], bgra[1], bgra[0], bgra[3]]).collect(),
    })
}

/// Where cursor themes are looked for: the system's own list, or the usual
/// places when it gives none.
fn theme_roots() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match std::env::var_os("XCURSOR_PATH") {
        Some(paths) => std::env::split_paths(&paths)
            .map(|path| match (path.strip_prefix("~"), &home) {
                (Ok(rest), Some(home)) => home.join(rest),
                _ => path,
            })
            .collect(),
        None => home
            .iter()
            .flat_map(|home| [home.join(".icons"), home.join(".local/share/icons")])
            .chain(["/usr/share/icons".into(), "/usr/local/share/icons".into()])
            .collect(),
    }
}

/// The file of cursor `name`: Bibata Modern Classic's when it is installed,
/// or the one of the theme the user's desktop uses.
fn theme_file(name: &str) -> Option<Vec<u8>> {
    let themes = ["Bibata-Modern-Classic".to_owned()]
        .into_iter()
        .chain(std::env::var("XCURSOR_THEME").ok());
    let roots = theme_roots();
    themes
        .flat_map(|theme| roots.iter().map(move |root| root.join(&theme).join("cursors").join(name)))
        // A cursor file is a few hundred kilobytes; anything else is not one.
        .find(|file| file.metadata().is_ok_and(|file| file.is_file() && file.len() <= 8 * 1024 * 1024))
        .and_then(|file| std::fs::read(file).ok())
}

struct Sprite {
    texture: egui::TextureHandle,
    /// The hotspot and the size, in points.
    hot: egui::Vec2,
    size: egui::Vec2,
}

/// The agent's pointer as the window keeps it: the theme's pictures, read
/// once for the scale they are shown at.
#[derive(Default)]
pub(crate) struct AgentCursor {
    /// The scale the pictures were read for, and the arrow and text cursors.
    loaded: Option<(f32, Option<Sprite>, Option<Sprite>)>,
}

/// The size of a desktop's pointer, in points.
const POINTER_SIZE: f32 = 24.0;

impl AgentCursor {
    fn sprite(ctx: &egui::Context, names: &[&str], scale: f32) -> Option<Sprite> {
        let wanted = (POINTER_SIZE * scale).round() as u32;
        let image = names
            .iter()
            .find_map(|name| parse_xcursor(&theme_file(name)?, wanted))?;
        let size = egui::vec2(image.size[0] as f32, image.size[1] as f32) / scale;
        let picture = egui::ColorImage::from_rgba_premultiplied(image.size, &image.rgba);
        Some(Sprite {
            texture: ctx.load_texture(format!("agent-cursor:{}", names[0]), picture, egui::TextureOptions::LINEAR),
            hot: egui::vec2(image.hot.0, image.hot.1) / scale,
            size,
        })
    }

    /// Paints `cue` over the page whose viewport starts at `page`, inside
    /// `clip`, and asks for the next frame while it moves. With `reduced`
    /// motion it is only shown where it acts, and taken away.
    pub(crate) fn paint(&mut self, ui: &egui::Ui, page: egui::Rect, clip: egui::Rect, cue: &AgentCue, reduced: bool) {
        let ctx = ui.ctx();
        let now = Instant::now();
        let Some(mut pose) = cue.pose(now, (page.width() / 2.0, page.height() / 2.0)) else {
            return;
        };
        if reduced {
            pose = Pose { alpha: 1.0, press: 0.0, ring: None, moving: false, ..pose };
        }
        if pose.moving {
            ctx.request_repaint();
        } else {
            // Its next change: the fade, or with less motion its end.
            let rest = if reduced { REST + FADE } else { REST };
            let until = cue.started + cue.travel + Duration::from_secs_f32(rest);
            ctx.request_repaint_after(until.saturating_duration_since(now));
        }
        let scale = ctx.pixels_per_point();
        if !matches!(&self.loaded, Some((loaded, ..)) if *loaded == scale) {
            self.loaded = Some((
                scale,
                Self::sprite(ctx, &["left_ptr", "default"], scale),
                Self::sprite(ctx, &["xterm", "text"], scale),
            ));
        }
        let painter = ui.painter().with_clip_rect(clip);
        let at = page.min + egui::vec2(pose.at.0, pose.at.1);
        let shade = |alpha: f32| (255.0 * alpha * pose.alpha).round() as u8;
        if let Some((radius, alpha)) = pose.ring {
            // Light over dark: it reads on any page.
            painter.circle_stroke(at, radius + 1.0, egui::Stroke::new(1.0, egui::Color32::from_black_alpha(shade(alpha * 0.6))));
            painter.circle_stroke(at, radius, egui::Stroke::new(2.0, egui::Color32::from_white_alpha(shade(alpha))));
        }
        let (_, arrow, beam) = self.loaded.as_ref().expect("loaded above");
        let writing = cue.kind == Cue::Type && pose.arrived;
        match beam.as_ref().filter(|_| writing).or(arrow.as_ref()) {
            Some(sprite) => {
                // A press shrinks it towards its tip.
                let pressed = 1.0 - 0.14 * pose.press;
                let rect = egui::Rect::from_min_size(at - sprite.hot * pressed, sprite.size * pressed);
                let whole = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                painter.image(sprite.texture.id(), rect, whole, egui::Color32::from_white_alpha(shade(1.0)));
            }
            // No theme to read: a dot says where the agent acts.
            None => {
                let radius = 6.0 - pose.press;
                painter.circle_filled(at, radius, egui::Color32::from_white_alpha(shade(0.95)));
                painter.circle_stroke(at, radius, egui::Stroke::new(1.5, egui::Color32::from_black_alpha(shade(0.8))));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An Xcursor file with one opaque picture per nominal size, each as
    /// wide as its size and filled with `[blue, green, red, alpha]`.
    fn xcursor(sizes: &[u32], bgra: [u8; 4]) -> Vec<u8> {
        let mut file = b"Xcur".to_vec();
        let header = 16 + sizes.len() * 12;
        for word in [16, 0x1_0000, sizes.len() as u32] {
            file.extend(word.to_le_bytes());
        }
        let mut at = header;
        for size in sizes {
            for word in [XCURSOR_IMAGE, *size, at as u32] {
                file.extend(word.to_le_bytes());
            }
            at += 36 + (size * size * 4) as usize;
        }
        for size in sizes {
            for word in [36, XCURSOR_IMAGE, *size, 1, *size, *size, 3, 5, 0] {
                file.extend(word.to_le_bytes());
            }
            file.extend(bgra.repeat((size * size) as usize));
        }
        file
    }

    #[test]
    fn a_cursor_file_gives_the_picture_for_the_scale_and_nothing_when_it_is_not_one() {
        let file = xcursor(&[24, 32, 48, 64], [10, 20, 30, 255]);
        let sized = |wanted| parse_xcursor(&file, wanted).unwrap().size[0];
        // The smallest that is not under what the scale asks for, else the largest.
        assert_eq!([sized(24), sized(36), sized(48), sized(49), sized(200), sized(1)], [24, 48, 48, 64, 64, 24]);
        let image = parse_xcursor(&file, 48).unwrap();
        assert_eq!((image.size, image.hot), ([48, 48], (3.0, 5.0)));
        // Blue first in the file, red first for the texture.
        assert_eq!(image.rgba[..4], [30, 20, 10, 255]);
        assert_eq!(image.rgba.len(), 48 * 48 * 4);
        // The order of the sizes in the file does not matter.
        assert_eq!(parse_xcursor(&xcursor(&[64, 24, 48], [0; 4]), 30).unwrap().size[0], 48);

        assert_eq!(parse_xcursor(b"PNG\x89 not a cursor", 24), None);
        assert_eq!(parse_xcursor(&file[..file.len() - 1], 64), None, "a picture cut short");
        assert_eq!(parse_xcursor(&xcursor(&[600], [0; 4]), 24), None, "larger than any cursor");
        assert_eq!(parse_xcursor(&xcursor(&[], [0; 4]), 24), None);
    }

    /// What the installed theme gives, laid over a light and a dark page as
    /// the window blends it: for a person to look at.
    #[test]
    #[ignore = "writes a picture of this machine's cursor theme for review"]
    fn export_the_themes_pointer_for_review() {
        let (arrow, beam) = (
            parse_xcursor(&theme_file("left_ptr").expect("no cursor theme here"), 48).unwrap(),
            parse_xcursor(&theme_file("xterm").unwrap(), 48).unwrap(),
        );
        let (width, height) = (480usize, 160usize);
        let mut sheet = vec![0u8; width * height * 4];
        for (index, pixel) in sheet.chunks_exact_mut(4).enumerate() {
            let shade = if index % width < width / 2 { 246 } else { 24 };
            pixel.copy_from_slice(&[shade, shade, shade, 255]);
        }
        let mut lay = |image: &CursorImage, left: usize, top: usize, alpha: f32| {
            for y in 0..image.size[1] {
                for x in 0..image.size[0] {
                    let from = &image.rgba[(y * image.size[0] + x) * 4..][..4];
                    let to = &mut sheet[((top + y) * width + left + x) * 4..][..4];
                    // Premultiplied over opaque, with the window's tint.
                    let cover = f32::from(from[3]) / 255.0 * alpha;
                    for channel in 0..3 {
                        to[channel] = (f32::from(from[channel]) * alpha + f32::from(to[channel]) * (1.0 - cover)).round() as u8;
                    }
                }
            }
        };
        for half in [0, width / 2] {
            lay(&arrow, half + 20, 20, 1.0);
            lay(&arrow, half + 90, 20, 0.4);
            lay(&beam, half + 160, 20, 1.0);
        }
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/openade-agent-cursor-review");
        std::fs::create_dir_all(&out).unwrap();
        image::save_buffer(out.join("theme.png"), &sheet, width as u32, height as u32, image::ColorType::Rgba8).unwrap();
        eprintln!("arrow {:?} hot {:?}, text {:?} hot {:?}", arrow.size, arrow.hot, beam.size, beam.hot);
    }

    #[test]
    fn the_pointer_glides_to_where_the_agent_acts_pulses_there_and_fades() {
        let start = Instant::now();
        let at = |cue: &AgentCue, seconds: f32| cue.pose(start + Duration::from_secs_f32(seconds), (500.0, 400.0));

        // The first time it comes in from the middle of the page.
        let first = AgentCue::new(Cue::Click, None, (100.0, 100.0), start);
        let begins = at(&first, 0.0).unwrap();
        assert_eq!((begins.at, begins.alpha, begins.arrived), ((500.0, 400.0), 0.0, false));
        let travel = first.travel.as_secs_f32();
        let halfway = at(&first, travel / 2.0).unwrap();
        assert!(halfway.at.0 > 100.0 && halfway.at.0 < 500.0 && halfway.moving && halfway.ring.is_none());
        assert!((halfway.at.0 - 300.0).abs() < 0.5, "eased evenly about the middle: {halfway:?}");

        // It is there when the action lands, and the click shows.
        let lands = at(&first, travel + 0.05).unwrap();
        assert_eq!((lands.at, lands.alpha, lands.arrived), ((100.0, 100.0), 1.0, true));
        let (radius, opacity) = lands.ring.unwrap();
        assert!(radius > 6.0 && opacity > 0.0 && lands.press > 0.5 && lands.moving);
        // Then it rests, without asking for frames, and fades away.
        let rests = at(&first, travel + 1.0).unwrap();
        assert_eq!((rests.ring, rests.press, rests.alpha, rests.moving), (None, 0.0, 1.0, false));
        let fading = at(&first, travel + REST + FADE / 2.0).unwrap();
        assert!(fading.alpha > 0.0 && fading.alpha < 1.0 && fading.moving);
        assert_eq!(at(&first, travel + REST + FADE + 0.01), None);

        // The next action leaves from where the last one was, sooner when near.
        let near = AgentCue::new(Cue::Move, Some((100.0, 100.0)), (130.0, 140.0), start);
        let far = AgentCue::new(Cue::Type, Some((100.0, 100.0)), (1500.0, 1100.0), start);
        assert!(near.travel < far.travel && far.travel <= Duration::from_millis(450));
        assert!(near.travel > Duration::from_millis(119));
        let there = AgentCue::new(Cue::Click, Some((100.0, 100.0)), (100.0, 100.0), start);
        assert!(there.travel.is_zero() && at(&there, 0.01).unwrap().ring.is_some(), "a second click where it is");
        assert_eq!(at(&near, 0.0).unwrap().at, (100.0, 100.0));
        assert_eq!(at(&near, 0.0).unwrap().alpha, 1.0, "it was already on the page");
        // A hover moves the pointer and clicks nothing.
        assert_eq!(at(&near, near.travel.as_secs_f32() + 0.05).unwrap().ring, None);
        assert!(at(&far, far.travel.as_secs_f32() + 0.05).unwrap().ring.is_some());
    }
}
