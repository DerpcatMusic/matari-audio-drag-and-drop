/// One normalized MIDI note in a drag preview.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MidiPreviewNote {
    /// Note start, where `0.0` is the left edge and `1.0` is the right edge.
    pub start: f32,
    /// Note end in the same normalized time range.
    pub end: f32,
    /// Pitch, where `0.0` is the bottom and `1.0` is the top.
    pub pitch: f32,
}

/// Optional source-side image data for an outbound file drag.
#[derive(Clone, Debug, PartialEq)]
pub enum DragPreview {
    /// Min/max waveform buckets in normalized audio amplitude.
    Waveform {
        /// Ordered `(minimum, maximum)` sample buckets.
        buckets: Vec<(f32, f32)>,
    },
    /// Column-major normalized spectral energy.
    Spectral {
        /// Number of time columns.
        columns: usize,
        /// Number of frequency rows.
        rows: usize,
        /// Energy at `column * rows + row`, clamped to `0.0..=1.0` when drawn.
        energy: Vec<f32>,
    },
    /// Normalized piano-roll notes.
    Midi {
        /// Notes to draw in the source preview.
        notes: Vec<MidiPreviewNote>,
    },
}

pub(crate) const WIDTH: usize = 224;
pub(crate) const HEIGHT: usize = 90;

pub(crate) fn render(preview: &DragPreview) -> Vec<u8> {
    let mut pixels = vec![0_u8; WIDTH * HEIGHT * 4];
    fill_rect(&mut pixels, 3, 4, WIDTH - 8, HEIGHT - 9, [18, 16, 28, 255]);
    fill_rect(
        &mut pixels,
        9,
        10,
        WIDTH - 20,
        HEIGHT - 21,
        [13, 19, 30, 255],
    );
    stroke_rect(
        &mut pixels,
        3,
        4,
        WIDTH - 8,
        HEIGHT - 9,
        [95, 110, 132, 255],
    );

    match preview {
        DragPreview::Waveform { buckets } => waveform(&mut pixels, buckets),
        DragPreview::Spectral {
            columns,
            rows,
            energy,
        } => spectral(&mut pixels, *columns, *rows, energy),
        DragPreview::Midi { notes } => midi(&mut pixels, notes),
    }
    pixels
}

fn waveform(pixels: &mut [u8], buckets: &[(f32, f32)]) {
    const LEFT: usize = 15;
    const RIGHT: usize = WIDTH - 16;
    const TOP: usize = 15;
    const BOTTOM: usize = HEIGHT - 17;
    let center = (TOP + BOTTOM) / 2;
    horizontal(pixels, LEFT, RIGHT, center, [75, 98, 124, 255]);
    let last = buckets.len().saturating_sub(1).max(1);
    for (index, &(minimum, maximum)) in buckets.iter().enumerate() {
        let x = LEFT + index * (RIGHT - LEFT) / last;
        let amplitude = (BOTTOM - TOP) as f32 * 0.42;
        let a = (center as f32 - maximum.clamp(-1.0, 1.0) * amplitude)
            .round()
            .clamp(TOP as f32, BOTTOM as f32) as usize;
        let b = (center as f32 - minimum.clamp(-1.0, 1.0) * amplitude)
            .round()
            .clamp(TOP as f32, BOTTOM as f32) as usize;
        fill_rect(
            pixels,
            x,
            a.min(b),
            2,
            a.max(b).saturating_sub(a.min(b)).max(1),
            [169, 222, 255, 255],
        );
    }
}

fn spectral(pixels: &mut [u8], columns: usize, rows: usize, energy: &[f32]) {
    if columns == 0 || rows == 0 {
        return;
    }
    const LEFT: usize = 14;
    const TOP: usize = 14;
    const DRAW_WIDTH: usize = WIDTH - 28;
    const DRAW_HEIGHT: usize = HEIGHT - 28;
    for y in 0..DRAW_HEIGHT {
        let row = rows
            .saturating_sub(1)
            .saturating_sub(y * rows / DRAW_HEIGHT);
        for x in 0..DRAW_WIDTH {
            let column = x * columns / DRAW_WIDTH;
            let value = energy
                .get(column.saturating_mul(rows).saturating_add(row))
                .copied()
                .unwrap_or_default()
                .clamp(0.0, 1.0);
            set_pixel(pixels, LEFT + x, TOP + y, spectral_color(value));
        }
    }
}

fn midi(pixels: &mut [u8], notes: &[MidiPreviewNote]) {
    const LEFT: f32 = 14.0;
    const TOP: f32 = 14.0;
    const DRAW_WIDTH: f32 = (WIDTH - 28) as f32;
    const DRAW_HEIGHT: f32 = (HEIGHT - 28) as f32;
    if notes.is_empty() {
        horizontal(
            pixels,
            LEFT as usize,
            (LEFT + DRAW_WIDTH) as usize,
            (TOP + DRAW_HEIGHT * 0.5) as usize,
            [55, 72, 92, 255],
        );
        return;
    }
    for note in notes.iter().take(96) {
        let start = note.start.clamp(0.0, 1.0);
        let end = note.end.clamp(start + 0.01, 1.0);
        let height = (DRAW_HEIGHT / 18.0).clamp(3.0, 7.0);
        fill_rect(
            pixels,
            (LEFT + start * DRAW_WIDTH).round() as usize,
            (TOP + (1.0 - note.pitch.clamp(0.0, 1.0)) * (DRAW_HEIGHT - height)).round() as usize,
            ((end - start) * DRAW_WIDTH).round().max(2.0) as usize,
            height.round() as usize,
            [120, 210, 190, 255],
        );
    }
}

fn spectral_color(value: f32) -> [u8; 4] {
    let (from, to, amount) = if value < 0.55 {
        ([45.0, 54.0, 82.0], [49.0, 180.0, 178.0], value / 0.55)
    } else {
        (
            [49.0, 180.0, 178.0],
            [247.0, 214.0, 112.0],
            (value - 0.55) / 0.45,
        )
    };
    [
        (from[0] + (to[0] - from[0]) * amount).round() as u8,
        (from[1] + (to[1] - from[1]) * amount).round() as u8,
        (from[2] + (to[2] - from[2]) * amount).round() as u8,
        255,
    ]
}

fn fill_rect(pixels: &mut [u8], x: usize, y: usize, width: usize, height: usize, color: [u8; 4]) {
    for row in y..y.saturating_add(height).min(HEIGHT) {
        for column in x..x.saturating_add(width).min(WIDTH) {
            set_pixel(pixels, column, row, color);
        }
    }
}

fn stroke_rect(pixels: &mut [u8], x: usize, y: usize, width: usize, height: usize, color: [u8; 4]) {
    horizontal(pixels, x, x + width - 1, y, color);
    horizontal(pixels, x, x + width - 1, y + height - 1, color);
    fill_rect(pixels, x, y, 1, height, color);
    fill_rect(pixels, x + width - 1, y, 1, height, color);
}

fn horizontal(pixels: &mut [u8], left: usize, right: usize, y: usize, color: [u8; 4]) {
    for x in left..=right.min(WIDTH - 1) {
        set_pixel(pixels, x, y, color);
    }
}

fn set_pixel(pixels: &mut [u8], x: usize, y: usize, rgba: [u8; 4]) {
    if x >= WIDTH || y >= HEIGHT {
        return;
    }
    let offset = (y * WIDTH + x) * 4;
    pixels[offset..offset + 4].copy_from_slice(&[rgba[2], rgba[1], rgba[0], rgba[3]]);
}
