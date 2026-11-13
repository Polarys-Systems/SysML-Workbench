//! Single-channel glyph atlas: shelf packing into a CPU image plus a dirty
//! rectangle that the renderer uploads to the GPU texture.

/// Empty texels kept around every glyph so linear sampling never bleeds.
const PADDING: u32 = 1;

struct Shelf {
    y: u32,
    height: u32,
    /// Next free x.
    x: u32,
}

pub struct Atlas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    shelves: Vec<Shelf>,
    /// Top of the unused area below the last shelf.
    next_y: u32,
    /// `[x0, y0, x1, y1]` changed since the last upload.
    dirty: Option<[u32; 4]>,
}

impl Atlas {
    /// A cleared atlas, fully dirty so the first upload initializes the texture.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width as usize * height as usize],
            shelves: Vec::new(),
            next_y: PADDING,
            dirty: Some([0, 0, width, height]),
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Forgets every glyph.
    pub fn clear(&mut self) {
        *self = Self::new(self.width, self.height);
    }

    /// Reserves a `width`x`height` rectangle; `None` when the atlas is full.
    /// Prefers the shelf that wastes the least height.
    pub fn allocate(&mut self, width: u32, height: u32) -> Option<[u32; 2]> {
        let (w, h) = (width + PADDING, height + PADDING);
        if w + PADDING > self.width || h + PADDING > self.height {
            return None;
        }
        let best = self
            .shelves
            .iter_mut()
            .filter(|s| s.height >= h && s.height <= h + h / 2 + 2 && s.x + w <= self.width)
            .min_by_key(|s| s.height);
        if let Some(shelf) = best {
            let at = [shelf.x, shelf.y];
            shelf.x += w;
            return Some(at);
        }
        // Round shelf heights so nearby sizes share shelves.
        let shelf_height = h.next_multiple_of(4).min(self.height - self.next_y);
        if shelf_height < h || self.next_y + shelf_height > self.height {
            return None;
        }
        let at = [PADDING, self.next_y];
        self.shelves.push(Shelf { y: self.next_y, height: shelf_height, x: PADDING + w });
        self.next_y += shelf_height;
        Some(at)
    }

    /// The writable region starting at `(x, y)` with rows `width()` bytes apart.
    /// Marks `w`x`h` texels there as changed.
    pub fn region_mut(&mut self, x: u32, y: u32, w: u32, h: u32) -> &mut [u8] {
        let r = [x, y, x + w, y + h];
        self.dirty = Some(match self.dirty {
            Some(d) => [d[0].min(r[0]), d[1].min(r[1]), d[2].max(r[2]), d[3].max(r[3])],
            None => r,
        });
        let start = y as usize * self.width as usize + x as usize;
        &mut self.pixels[start..]
    }

    /// Takes the changed rectangle `[x, y, w, h]` and its tightly packed texels.
    pub fn take_dirty(&mut self) -> Option<([u32; 4], Vec<u8>)> {
        let [x0, y0, x1, y1] = self.dirty.take()?;
        let (w, h) = (x1 - x0, y1 - y0);
        if w == 0 || h == 0 {
            return None;
        }
        let mut bytes = Vec::with_capacity(w as usize * h as usize);
        for row in y0..y1 {
            let start = row as usize * self.width as usize + x0 as usize;
            bytes.extend_from_slice(&self.pixels[start..start + w as usize]);
        }
        Some(([x0, y0, w, h], bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_without_overlap_and_reports_full() {
        let mut atlas = Atlas::new(64, 64);
        let mut rects = Vec::new();
        while let Some([x, y]) = atlas.allocate(7, 9) {
            rects.push([x, y, x + 7, y + 9]);
        }
        assert!(rects.len() > 20);
        for (i, a) in rects.iter().enumerate() {
            assert!(a[2] < 64 && a[3] < 64);
            for b in &rects[i + 1..] {
                let overlap = a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3];
                assert!(!overlap, "{a:?} overlaps {b:?}");
            }
        }
        assert!(atlas.allocate(100, 1).is_none());
    }

    #[test]
    fn dirty_region_is_tight() {
        let mut atlas = Atlas::new(16, 16);
        atlas.take_dirty();
        let [x, y] = atlas.allocate(3, 2).unwrap();
        atlas.region_mut(x, y, 3, 2)[0] = 255;
        let (rect, bytes) = atlas.take_dirty().unwrap();
        assert_eq!(rect, [x, y, 3, 2]);
        assert_eq!(bytes.len(), 6);
        assert_eq!(bytes[0], 255);
        assert!(atlas.take_dirty().is_none());
    }
}
