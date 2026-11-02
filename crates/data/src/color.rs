//! Colours written as text: CSS colours (`#rrggbb`, `rgb()`, `hsl()`, named) and the
//! `r,g,b[,a]` text expressions give (`color_rgb`, `color_rgba`).

/// Straight (not premultiplied) RGBA.
pub type Rgba = [u8; 4];

/// A colour as an expression gives it: `r,g,b` or `r,g,b,a` (0–255 each, as `color_rgb` and
/// `color_rgba` write it), else a CSS colour ([`parse`]).
pub fn parse_value(s: &str) -> Option<Rgba> {
    let t = s.trim();
    if t.len() <= 64 && t.contains(',') && !t.contains('(') {
        let parts: Vec<&str> = t.split(',').map(str::trim).collect();
        if (3..=4).contains(&parts.len()) {
            let ch = |p: &&str| p.parse::<f64>().ok().filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 255.0).round() as u8);
            let mut c = [255u8; 4];
            for (slot, p) in c.iter_mut().zip(&parts) {
                *slot = ch(p)?;
            }
            return Some(c);
        }
        return None;
    }
    parse(t)
}

/// Parses a CSS color: `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb[a]()`, `hsl[a]()` (comma or space syntax,
/// `/ alpha`, `%` alpha), `transparent` and the 148 CSS named colors. Case-insensitive.
pub fn parse(s: &str) -> Option<Rgba> {
    let s = s.trim();
    if s.is_empty() || s.len() > 128 {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    if let Some(hex) = lower.strip_prefix('#') {
        return parse_hex(hex);
    }
    if let Some((name, rest)) = lower.split_once('(') {
        let inner = rest.trim_end().strip_suffix(')')?;
        let parts: Vec<&str> = inner.split(|c: char| c == ',' || c == '/' || c.is_whitespace()).filter(|p| !p.is_empty()).collect();
        if !(3..=4).contains(&parts.len()) {
            return None;
        }
        let alpha = match parts.get(3) {
            Some(a) => alpha_u8(a)?,
            None => 255,
        };
        let (a, b, c) = (parts.first()?, parts.get(1)?, parts.get(2)?);
        return match name.trim() {
            "rgb" | "rgba" => Some([channel(a)?, channel(b)?, channel(c)?, alpha]),
            "hsl" | "hsla" => {
                let [r, g, b] = hsl_to_rgb(hue(a)?, pct(b)?, pct(c)?);
                Some([r, g, b, alpha])
            }
            _ => None,
        };
    }
    if lower == "transparent" {
        return Some([0, 0, 0, 0]);
    }
    let i = NAMED_COLORS.binary_search_by(|(n, _)| (*n).cmp(lower.as_str())).ok()?;
    let [_, r, g, b] = NAMED_COLORS.get(i)?.1.to_be_bytes();
    Some([r, g, b, 255])
}

fn parse_hex(h: &str) -> Option<Rgba> {
    if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let digit = |i: usize, n: usize| h.get(i..i + n).and_then(|d| u8::from_str_radix(d, 16).ok());
    let mut c = [255u8; 4];
    match h.len() {
        3 | 4 => {
            for (i, slot) in c.iter_mut().enumerate().take(h.len()) {
                *slot = digit(i, 1)? * 17;
            }
        }
        6 | 8 => {
            for (i, slot) in c.iter_mut().enumerate().take(h.len() / 2) {
                *slot = digit(i * 2, 2)?;
            }
        }
        _ => return None,
    }
    Some(c)
}

fn css_num(s: &str) -> Option<f64> {
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// A fraction 0..1 as a channel 0..255.
pub fn unit_u8(v: f64) -> u8 {
    (v * 255.0).clamp(0.0, 255.0).round() as u8
}

fn channel(s: &str) -> Option<u8> {
    let v = match s.strip_suffix('%') {
        Some(p) => css_num(p)? / 100.0,
        None => css_num(s)? / 255.0,
    };
    Some(unit_u8(v))
}

fn alpha_u8(s: &str) -> Option<u8> {
    let v = match s.strip_suffix('%') {
        Some(p) => css_num(p)? / 100.0,
        None => css_num(s)?,
    };
    Some(unit_u8(v))
}

fn hue(s: &str) -> Option<f64> {
    Some(css_num(s.strip_suffix("deg").unwrap_or(s))?.rem_euclid(360.0))
}

fn pct(s: &str) -> Option<f64> {
    Some((css_num(s.strip_suffix('%').unwrap_or(s))? / 100.0).clamp(0.0, 1.0))
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [unit_u8(r + m), unit_u8(g + m), unit_u8(b + m)]
}

/// CSS named colors, sorted by name, as 0xRRGGBB.
#[rustfmt::skip]
static NAMED_COLORS: [(&str, u32); 148] = [
    ("aliceblue", 0xf0f8ff), ("antiquewhite", 0xfaebd7), ("aqua", 0x00ffff), ("aquamarine", 0x7fffd4), ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc), ("bisque", 0xffe4c4), ("black", 0x000000), ("blanchedalmond", 0xffebcd), ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2), ("brown", 0xa52a2a), ("burlywood", 0xdeb887), ("cadetblue", 0x5f9ea0), ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e), ("coral", 0xff7f50), ("cornflowerblue", 0x6495ed), ("cornsilk", 0xfff8dc), ("crimson", 0xdc143c),
    ("cyan", 0x00ffff), ("darkblue", 0x00008b), ("darkcyan", 0x008b8b), ("darkgoldenrod", 0xb8860b), ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400), ("darkgrey", 0xa9a9a9), ("darkkhaki", 0xbdb76b), ("darkmagenta", 0x8b008b), ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00), ("darkorchid", 0x9932cc), ("darkred", 0x8b0000), ("darksalmon", 0xe9967a), ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b), ("darkslategray", 0x2f4f4f), ("darkslategrey", 0x2f4f4f), ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3), ("deeppink", 0xff1493), ("deepskyblue", 0x00bfff), ("dimgray", 0x696969), ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff), ("firebrick", 0xb22222), ("floralwhite", 0xfffaf0), ("forestgreen", 0x228b22), ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc), ("ghostwhite", 0xf8f8ff), ("gold", 0xffd700), ("goldenrod", 0xdaa520), ("gray", 0x808080),
    ("green", 0x008000), ("greenyellow", 0xadff2f), ("grey", 0x808080), ("honeydew", 0xf0fff0), ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c), ("indigo", 0x4b0082), ("ivory", 0xfffff0), ("khaki", 0xf0e68c), ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5), ("lawngreen", 0x7cfc00), ("lemonchiffon", 0xfffacd), ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080), ("lightcyan", 0xe0ffff), ("lightgoldenrodyellow", 0xfafad2), ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90), ("lightgrey", 0xd3d3d3), ("lightpink", 0xffb6c1), ("lightsalmon", 0xffa07a), ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa), ("lightslategray", 0x778899), ("lightslategrey", 0x778899), ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0), ("lime", 0x00ff00), ("limegreen", 0x32cd32), ("linen", 0xfaf0e6), ("magenta", 0xff00ff),
    ("maroon", 0x800000), ("mediumaquamarine", 0x66cdaa), ("mediumblue", 0x0000cd), ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db), ("mediumseagreen", 0x3cb371), ("mediumslateblue", 0x7b68ee), ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc), ("mediumvioletred", 0xc71585), ("midnightblue", 0x191970), ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1), ("moccasin", 0xffe4b5), ("navajowhite", 0xffdead), ("navy", 0x000080), ("oldlace", 0xfdf5e6),
    ("olive", 0x808000), ("olivedrab", 0x6b8e23), ("orange", 0xffa500), ("orangered", 0xff4500), ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa), ("palegreen", 0x98fb98), ("paleturquoise", 0xafeeee), ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5), ("peachpuff", 0xffdab9), ("peru", 0xcd853f), ("pink", 0xffc0cb), ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6), ("purple", 0x800080), ("rebeccapurple", 0x663399), ("red", 0xff0000), ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1), ("saddlebrown", 0x8b4513), ("salmon", 0xfa8072), ("sandybrown", 0xf4a460), ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee), ("sienna", 0xa0522d), ("silver", 0xc0c0c0), ("skyblue", 0x87ceeb), ("slateblue", 0x6a5acd),
    ("slategray", 0x708090), ("slategrey", 0x708090), ("snow", 0xfffafa), ("springgreen", 0x00ff7f), ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c), ("teal", 0x008080), ("thistle", 0xd8bfd8), ("tomato", 0xff6347), ("turquoise", 0x40e0d0), ("violet", 0xee82ee),
    ("wheat", 0xf5deb3), ("white", 0xffffff), ("whitesmoke", 0xf5f5f5), ("yellow", 0xffff00), ("yellowgreen", 0x9acd32),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(parse("#f00"), Some([255, 0, 0, 255]));
        assert_eq!(parse("#F008"), Some([255, 0, 0, 136]));
        assert_eq!(parse("#12ab3C"), Some([0x12, 0xab, 0x3c, 255]));
        assert_eq!(parse("#12ab3c80"), Some([0x12, 0xab, 0x3c, 0x80]));
        assert_eq!(parse("#12ab3"), None);
        assert_eq!(parse("#ggg"), None);
        assert_eq!(parse("rgb(255, 128, 0)"), Some([255, 128, 0, 255]));
        assert_eq!(parse("rgba( 10 , 20 , 30 , 0.5 )"), Some([10, 20, 30, 128]));
        assert_eq!(parse("rgba(10,20,30,50%)"), Some([10, 20, 30, 128]));
        assert_eq!(parse("rgb(10 20 30 / 0.25)"), Some([10, 20, 30, 64]));
        assert_eq!(parse("rgb(100%, 0%, 50%)"), Some([255, 0, 128, 255]));
        assert_eq!(parse("hsl(0, 100%, 50%)"), Some([255, 0, 0, 255]));
        assert_eq!(parse("hsl(120,100%,25%)"), Some([0, 128, 0, 255]));
        assert_eq!(parse("hsla(240, 100%, 50%, 0.5)"), Some([0, 0, 255, 128]));
        assert_eq!(parse("hsl(240deg 100% 50% / 50%)"), Some([0, 0, 255, 128]));
        assert_eq!(parse("hsla(0,3%,85%,0.84)"), Some([218, 216, 216, 214]));
        assert_eq!(parse("Red"), Some([255, 0, 0, 255]));
        assert_eq!(parse("rebeccapurple"), Some([0x66, 0x33, 0x99, 255]));
        assert_eq!(parse("yellowgreen"), Some([0x9a, 0xcd, 0x32, 255]));
        assert_eq!(parse(" transparent "), Some([0, 0, 0, 0]));
        for bad in ["", "rgb(1,2)", "rgb(1,2,3,4,5)", "foo(1,2,3)", "rgb(a,b,c)", "nocolor", "rgb(1,2,3", "#"] {
            assert_eq!(parse(bad), None, "{bad}");
        }
        assert!(NAMED_COLORS.windows(2).all(|w| w[0].0 < w[1].0), "table sorted");
        assert!(NAMED_COLORS.iter().all(|(n, _)| parse(n).is_some()));
    }

    #[test]
    fn expression_colours() {
        assert_eq!(parse_value("255,0,0"), Some([255, 0, 0, 255]));
        assert_eq!(parse_value(" 10, 20 ,30, 128 "), Some([10, 20, 30, 128]));
        assert_eq!(parse_value("300,-4,1.6"), Some([255, 0, 2, 255]));
        assert_eq!(parse_value("#00ff00"), Some([0, 255, 0, 255]));
        assert_eq!(parse_value("rgb(1, 2, 3)"), Some([1, 2, 3, 255]));
        assert_eq!(parse_value("navy"), Some([0, 0, 128, 255]));
        for bad in ["", "1,2", "1,2,3,4,5", "a,b,c", "1,2,NaN", "1,,3", "red,green,blue"] {
            assert_eq!(parse_value(bad), None, "{bad}");
        }
    }
}
