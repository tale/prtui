//! Inline images for review comments, drawn with the kitty graphics protocol.
//!
//! Ratatui owns the cell grid, so an image is laid out as a run of blank rows
//! and painted over them afterwards: the store hands back the escape sequences
//! for a frame and the terminal writes them inside the same synchronized update.

use anyhow::{Context, Result, bail};
use image::ImageFormat;
use image::imageops::FilterType;
use std::collections::HashMap;
use std::io::Cursor;

/// Kitty accepts at most 4096 base64 bytes per escape sequence.
const TRANSMIT_CHUNK: usize = 4096;

/// Comment screenshots are routinely far larger than any terminal viewport;
/// re-encoding smaller keeps both the transfer and the terminal's scaling cheap.
const MAX_DIMENSION: u32 = 1600;

/// Terminals that do not report pixel dimensions still need an aspect ratio to
/// turn an image height into rows. Roughly matches a typical monospace cell.
const CELL_ASPECT: f32 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellSize {
    pub width: u16,
    pub height: u16,
}

/// A decoded, downscaled image ready to hand to the terminal.
pub struct Image {
    pub width: u32,
    pub height: u32,
    png: Vec<u8>,
}

enum Entry {
    Loading,
    Ready {
        id: u32,
        image: Image,
        is_transmitted: bool,
    },
    Failed(String),
}

/// Whether images may be drawn, and if not, why not. The reason reaches the
/// reader, so a comment that shows no picture explains itself.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    Enabled,
    /// Turned off by the operator.
    #[default]
    Disabled,
    /// The terminal answered the capability probe with a no.
    Unsupported,
}

pub enum Status<'a> {
    Off(Support),
    Loading,
    Failed(&'a str),
    Ready { cols: u16, rows: u16 },
}

/// Where one image lands this frame, in terminal cells. `skip_rows` is how much
/// of the image scrolled off the top of the thread, so only the remaining slice
/// of the source is drawn.
pub struct Placement {
    pub url: String,
    pub column: u16,
    pub row: u16,
    pub cols: u16,
    pub rows: u16,
    pub skip_rows: u16,
    pub total_rows: u16,
}

#[derive(Default)]
pub struct Images {
    support: Support,
    cell: Option<CellSize>,
    entries: HashMap<String, Entry>,
    pending: Vec<String>,
    next_id: u32,
    has_placements: bool,
}

impl Images {
    pub fn new(support: Support) -> Self {
        Self {
            support,
            ..Self::default()
        }
    }

    pub fn is_supported(&self) -> bool {
        self.support == Support::Enabled
    }

    pub const fn set_cell_size(&mut self, cell: Option<CellSize>) {
        self.cell = cell;
    }

    /// Queue a fetch the first time a URL becomes visible.
    pub fn request(&mut self, url: &str) {
        if !self.is_supported() || self.entries.contains_key(url) {
            return;
        }

        self.entries.insert(url.to_string(), Entry::Loading);
        self.pending.push(url.to_string());
    }

    pub fn take_pending(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending)
    }

    pub fn insert(&mut self, url: String, image: Result<Image, String>) {
        let entry = match image {
            Ok(image) => {
                self.next_id += 1;
                Entry::Ready {
                    id: self.next_id,
                    image,
                    is_transmitted: false,
                }
            }
            Err(error) => Entry::Failed(error),
        };

        self.entries.insert(url, entry);
    }

    pub fn status(&self, url: &str, max_cols: u16, max_rows: u16) -> Status<'_> {
        if !self.is_supported() {
            return Status::Off(self.support);
        }

        match self.entries.get(url) {
            None | Some(Entry::Loading) => Status::Loading,
            Some(Entry::Failed(error)) => Status::Failed(error),
            Some(Entry::Ready { image, .. }) => {
                let (cols, rows) = fit(image, self.cell, max_cols, max_rows);
                Status::Ready { cols, rows }
            }
        }
    }

    /// Escape sequences that repaint every visible image for one frame. Image
    /// data is transmitted once; later frames only move placements around.
    pub fn frame_commands(&mut self, placements: &[Placement]) -> String {
        if !self.is_supported() || (placements.is_empty() && !self.has_placements) {
            return String::new();
        }
        self.has_placements = !placements.is_empty();

        // Saved/restored around the block so ratatui keeps its own cursor, and
        // stale placements go first so scrolled-away images do not linger.
        let mut commands = String::from("\x1b7\x1b_Ga=d,d=a,q=2\x1b\\");
        for (index, placement) in placements.iter().enumerate() {
            let Some(Entry::Ready {
                id,
                image,
                is_transmitted,
            }) = self.entries.get_mut(&placement.url)
            else {
                continue;
            };

            if !*is_transmitted {
                push_transmit(&mut commands, *id, &image.png);
                *is_transmitted = true;
            }
            push_placement(&mut commands, *id, index as u32 + 1, image, placement);
        }
        commands.push_str("\x1b8");

        commands
    }
}

/// Decode, downscale, and re-encode as PNG, which the protocol takes directly.
pub fn decode(bytes: &[u8]) -> Result<Image> {
    if is_video(bytes) {
        bail!("video attachment");
    }

    let decoded = image::load_from_memory(bytes).context("unsupported image format")?;
    let scaled = if decoded.width().max(decoded.height()) > MAX_DIMENSION {
        decoded.resize(MAX_DIMENSION, MAX_DIMENSION, FilterType::Triangle)
    } else {
        decoded
    };

    let mut png = Vec::new();
    scaled
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .context("re-encoding image")?;

    Ok(Image {
        width: scaled.width(),
        height: scaled.height(),
        png,
    })
}

/// Half the attachments dropped into a review are screen recordings, and no
/// terminal will draw those; saying so beats a decoder error about magic bytes.
fn is_video(bytes: &[u8]) -> bool {
    let is_mpeg4 = bytes.get(4..8).is_some_and(|tag| tag == b"ftyp");
    let is_matroska = bytes.starts_with(b"\x1a\x45\xdf\xa3");

    is_mpeg4 || is_matroska
}

/// Cells the image occupies, preserving its aspect ratio inside the box. Without
/// a reported cell size the image is sized to the available width instead of its
/// natural resolution.
fn fit(image: &Image, cell: Option<CellSize>, max_cols: u16, max_rows: u16) -> (u16, u16) {
    if max_cols == 0 || max_rows == 0 || image.width == 0 || image.height == 0 {
        return (0, 0);
    }

    let (cell_width, cell_height) = match cell {
        Some(cell) if cell.width > 0 && cell.height > 0 => {
            (f32::from(cell.width), f32::from(cell.height))
        }
        _ => (1.0, CELL_ASPECT),
    };
    let width = image.width as f32;
    let height = image.height as f32;

    let cols = (width / cell_width).round().clamp(1.0, f32::from(max_cols));
    let rows = (height * cell_width * cols / (width * cell_height)).round();
    if rows <= f32::from(max_rows) {
        return (cols as u16, rows.max(1.0) as u16);
    }

    let rows = f32::from(max_rows);
    let cols = (width * cell_height * rows / (height * cell_width))
        .round()
        .clamp(1.0, f32::from(max_cols));
    (cols as u16, rows as u16)
}

fn push_transmit(commands: &mut String, id: u32, png: &[u8]) {
    let payload = base64(png);
    let mut offset = 0;

    while offset < payload.len() {
        let end = (offset + TRANSMIT_CHUNK).min(payload.len());
        let more = u8::from(end < payload.len());
        let chunk = &payload[offset..end];

        if offset == 0 {
            commands.push_str(&format!(
                "\x1b_Ga=t,i={id},f=100,t=d,q=2,m={more};{chunk}\x1b\\"
            ));
        } else {
            commands.push_str(&format!("\x1b_Gm={more};{chunk}\x1b\\"));
        }
        offset = end;
    }
}

fn push_placement(
    commands: &mut String,
    id: u32,
    placement_id: u32,
    image: &Image,
    placement: &Placement,
) {
    let top = source_row(image.height, placement.skip_rows, placement.total_rows);
    let bottom = source_row(
        image.height,
        placement.skip_rows + placement.rows,
        placement.total_rows,
    );
    let height = bottom.saturating_sub(top).max(1);

    commands.push_str(&format!(
        "\x1b[{};{}H\x1b_Ga=p,i={id},p={placement_id},c={},r={},x=0,y={top},w={},h={height},C=1,q=2\x1b\\",
        placement.row + 1,
        placement.column + 1,
        placement.cols,
        placement.rows,
        image.width,
    ));
}

fn source_row(height: u32, row: u16, total_rows: u16) -> u32 {
    if total_rows == 0 {
        return 0;
    }
    (u64::from(height) * u64::from(row) / u64::from(total_rows)) as u32
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut block = [0u8; 3];
        block[..chunk.len()].copy_from_slice(chunk);
        let bits = u32::from_be_bytes([0, block[0], block[1], block[2]]);

        for (position, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if position > chunk.len() {
                encoded.push('=');
                continue;
            }
            encoded.push(ALPHABET[(bits >> shift & 63) as usize] as char);
        }
    }

    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32) -> Image {
        Image {
            width,
            height,
            png: vec![1, 2, 3],
        }
    }

    #[test]
    fn encodes_base64_with_padding() {
        assert_eq!(
            base64(b"any carnal pleasure."),
            "YW55IGNhcm5hbCBwbGVhc3VyZS4="
        );
        assert_eq!(
            base64(b"any carnal pleasure"),
            "YW55IGNhcm5hbCBwbGVhc3VyZQ=="
        );
        assert_eq!(base64(b"any carnal pleasur"), "YW55IGNhcm5hbCBwbGVhc3Vy");
        assert_eq!(base64(b""), "");
    }

    #[test]
    fn fits_within_the_box_and_keeps_aspect() {
        let cell = Some(CellSize {
            width: 10,
            height: 20,
        });

        // 400x400 is 40x20 cells naturally, which fits.
        assert_eq!(fit(&image(400, 400), cell, 80, 40), (40, 20));

        // Too wide: clamped to the column budget, rows follow the ratio.
        assert_eq!(fit(&image(800, 400), cell, 40, 40), (40, 10));

        // Too tall: clamped to the row budget, columns follow the ratio.
        assert_eq!(fit(&image(400, 1600), cell, 80, 20), (10, 20));

        // Without a reported cell size the image fills the available width.
        assert_eq!(fit(&image(400, 200), None, 30, 40), (30, 8));
    }

    #[test]
    fn names_a_video_attachment_instead_of_blaming_the_decoder() {
        // A QuickTime upload, which is what a dragged screen recording becomes.
        let mut movie = vec![0, 0, 0, 20];
        movie.extend_from_slice(b"ftypqt  ");

        let error = decode(&movie).err().map(|error| error.to_string());
        assert_eq!(error.as_deref(), Some("video attachment"));
    }

    #[test]
    fn crops_the_source_to_the_visible_rows() {
        let mut images = Images::new(Support::Enabled);
        images.insert("u".into(), Ok(image(100, 200)));

        let commands = images.frame_commands(&[Placement {
            url: "u".into(),
            column: 4,
            row: 2,
            cols: 20,
            rows: 5,
            skip_rows: 5,
            total_rows: 10,
        }]);

        assert!(commands.contains("\x1b_Ga=t,i=1,f=100,t=d,q=2,m=0;"));
        assert!(commands.contains("\x1b[3;5H"));
        assert!(commands.contains("a=p,i=1,p=1,c=20,r=5,x=0,y=100,w=100,h=100,C=1,q=2"));

        // Data is transmitted once; later frames only re-place.
        let repeat = images.frame_commands(&[]);
        assert!(!repeat.contains("a=t"));
        assert!(repeat.contains("a=d,d=a"));
    }

    #[test]
    fn stays_silent_when_unsupported() {
        let mut images = Images::default();
        images.request("u");

        assert!(images.take_pending().is_empty());
        assert!(matches!(
            images.status("u", 10, 10),
            Status::Off(Support::Disabled)
        ));
        assert!(images.frame_commands(&[]).is_empty());
    }
}
