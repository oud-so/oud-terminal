//! A small map of where a station broadcasts from, drawn in braille dots.
//!
//! Land comes from OpenStreetMap-derived country boundaries (ODbL,
//! © OpenStreetMap contributors), queried point by point: each braille dot
//! asks "which country is here?". That also lets the station's own country
//! stand out from its neighbours, and gives a centre to point at when a
//! station has no coordinates of its own.

use std::collections::HashMap;

use country_boundaries::{BOUNDARIES_ODBL_360X180, CountryBoundaries, LatLon};

pub const ATTRIBUTION: &str = "© openstreetmap contributors";

/// Degrees of longitude across the map. Latitude follows from the shape.
const SPAN_LON: f64 = 40.0;

/// One drawn cell: the braille glyph, and whether any of its dots fall in
/// the highlighted country.
pub type Cell = (char, bool);

pub struct Map {
    boundaries: CountryBoundaries,
    centres: HashMap<String, Option<(f64, f64)>>,
}

impl Map {
    pub fn load() -> Option<Self> {
        let boundaries = CountryBoundaries::from_reader(BOUNDARIES_ODBL_360X180).ok()?;
        Some(Self { boundaries, centres: HashMap::new() })
    }

    fn country_at(&self, lat: f64, lon: f64) -> Option<&str> {
        let lon = (lon + 180.0).rem_euclid(360.0) - 180.0;
        let point = LatLon::new(lat.clamp(-89.9, 89.9), lon).ok()?;
        // Ids run from most to least specific ("US-TX", "US"); the last is the country.
        self.boundaries.ids(point).into_iter().last()
    }

    /// The middle of a country, found by sampling a one-degree grid.
    pub fn centre_of(&mut self, iso: &str) -> Option<(f64, f64)> {
        let iso = iso.to_ascii_uppercase();
        if let Some(known) = self.centres.get(&iso) {
            return *known;
        }
        let (mut lat_sum, mut lon_sum, mut n) = (0.0, 0.0, 0.0);
        for lat in -60..80 {
            for lon in -180..180 {
                let (lat, lon) = (lat as f64 + 0.5, lon as f64 + 0.5);
                if self.country_at(lat, lon) == Some(iso.as_str()) {
                    lat_sum += lat;
                    lon_sum += lon;
                    n += 1.0;
                }
            }
        }
        let centre = (n > 0.0).then(|| (lat_sum / n, lon_sum / n));
        self.centres.insert(iso, centre);
        centre
    }

    /// Draw `cols` × `rows` cells around `(lat, lon)`. Returns the cells and
    /// the cell the point falls in.
    pub fn render(
        &self,
        (lat, lon): (f64, f64),
        highlight: Option<&str>,
        cols: usize,
        rows: usize,
    ) -> (Vec<Vec<Cell>>, (usize, usize)) {
        let (dots_x, dots_y) = (cols * 2, rows * 4);
        let per_dot = SPAN_LON / dots_x as f64;
        // Braille dots are about twice as tall as wide in most fonts' cells,
        // so a dot covers the same distance either way.
        let top = lat + per_dot * dots_y as f64 / 2.0;
        let left = lon - SPAN_LON / 2.0;
        let highlight = highlight.map(str::to_ascii_uppercase);

        // Look every dot up once, with a one-dot margin so edges have neighbours.
        let (gw, gh) = (dots_x + 2, dots_y + 2);
        let mut grid: Vec<Option<&str>> = Vec::with_capacity(gw * gh);
        for gy in 0..gh {
            for gx in 0..gw {
                let plat = top - (gy as f64 - 0.5) * per_dot;
                let plon = left + (gx as f64 - 0.5) * per_dot;
                grid.push(self.country_at(plat, plon));
            }
        }
        let at = |x: usize, y: usize| grid[y * gw + x];

        // The station's country is filled in; everything else is drawn as
        // outlines (coasts and borders), which keeps the map light.
        let mut cells = vec![vec![('\u{2800}', false); cols]; rows];
        for dy in 0..dots_y {
            for dx in 0..dots_x {
                let (gx, gy) = (dx + 1, dy + 1);
                let Some(country) = at(gx, gy) else { continue };
                let home = highlight.as_deref() == Some(country);
                let edge = [at(gx - 1, gy), at(gx + 1, gy), at(gx, gy - 1), at(gx, gy + 1)]
                    .iter()
                    .any(|n| *n != Some(country));
                if !(home || edge) {
                    continue;
                }
                let cell = &mut cells[dy / 4][dx / 2];
                cell.0 = add_dot(cell.0, dx % 2, dy % 4);
                cell.1 |= home;
            }
        }
        (cells, (cols / 2, rows / 2))
    }
}

/// Set the dot at (column, row) of a braille cell.
fn add_dot(glyph: char, x: usize, y: usize) -> char {
    const BITS: [[u32; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
    char::from_u32(glyph as u32 | BITS[x][y]).unwrap_or(glyph)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_countries_and_their_centres() {
        let mut map = Map::load().expect("bundled data loads");
        assert_eq!(map.country_at(39.9, 32.85), Some("TR"));
        assert_eq!(map.country_at(30.0, -40.0), None);
        let (lat, lon) = map.centre_of("tr").expect("turkey has land");
        assert!((36.0..42.0).contains(&lat) && (26.0..45.0).contains(&lon), "{lat}, {lon}");
    }

    #[test]
    fn draws_land_and_highlights_the_country() {
        let map = Map::load().unwrap();
        let (cells, marker) = map.render((39.0, 35.0), Some("TR"), 30, 8);
        assert_eq!((cells.len(), cells[0].len()), (8, 30));
        assert!(cells.iter().flatten().any(|(_, home)| *home));
        assert!(cells.iter().flatten().any(|(c, home)| !home && *c != '\u{2800}'));
        assert_eq!(marker, (15, 4));
    }

    #[test]
    fn braille_dots_combine() {
        let full =
            (0..2).flat_map(|x| (0..4).map(move |y| (x, y))).fold('\u{2800}', |g, (x, y)| add_dot(g, x, y));
        assert_eq!(full, '⣿');
    }
}
