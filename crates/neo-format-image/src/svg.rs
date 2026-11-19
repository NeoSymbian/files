// SPDX-License-Identifier: Apache-2.0
//! A bounded SVG subset, ours over `xmlparser`, drawn with `tiny-skia`: `svg` (viewBox), `g` (transform), rect,
//! circle, ellipse, line, polyline, polygon and path; fill, stroke, stroke-width, the opacities, fill-rule; colours as
//! hex, rgb() or a few names. Text, gradients, filters, `use` and whatever is under `defs` don't draw. At most
//! [`MAX_ELEMENTS`] elements and [`MAX_DEPTH`] levels.

use crate::{Error, Format, Info, Pixels};
use alloc::vec::Vec;
use libm::{atan2f, cosf, fabsf, sinf, sqrtf};
use tiny_skia::{FillRule, Paint, Path, PathBuilder, Pixmap, Rect, Stroke, Transform};
use xmlparser::{ElementEnd, Token, Tokenizer};

/// The long side an SVG is drawn at.
pub const SIDE: u32 = 1024;
pub const MAX_ELEMENTS: usize = 20_000;
pub const MAX_DEPTH: usize = 16;

fn text(b: &[u8]) -> Option<&str> {
    let b = b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b);
    let s = core::str::from_utf8(b).ok()?;
    s.trim_start().starts_with('<').then_some(s)
}

fn num(s: &str) -> Option<f32> {
    let s = s.trim();
    let s = s.strip_suffix("px").unwrap_or(s);
    s.parse::<f32>().ok().filter(|v| v.is_finite())
}

/// The root's width, height and viewBox.
fn root(s: &str) -> Option<(f32, f32, [f32; 4])> {
    let mut in_svg = false;
    let (mut w, mut h, mut vb) = (None, None, None);
    for t in Tokenizer::from(s).take(4096) {
        match t.ok()? {
            Token::ElementStart { local, .. } => {
                if in_svg {
                    break;
                }
                in_svg = local.as_str() == "svg";
                if !in_svg {
                    return None;
                }
            }
            Token::Attribute { local, value, .. } if in_svg => match local.as_str() {
                "width" => w = num(value.as_str()),
                "height" => h = num(value.as_str()),
                "viewBox" => {
                    let v: Vec<f32> = numbers(value.as_str()).collect();
                    if v.len() == 4 && v[2] > 0.0 && v[3] > 0.0 {
                        vb = Some([v[0], v[1], v[2], v[3]]);
                    }
                }
                _ => {}
            },
            Token::ElementEnd { .. } if in_svg => break,
            _ => {}
        }
    }
    if !in_svg {
        return None;
    }
    let vb = vb.unwrap_or([0.0, 0.0, w.unwrap_or(300.0), h.unwrap_or(150.0)]);
    Some((w.unwrap_or(vb[2]), h.unwrap_or(vb[3]), vb))
}

pub fn probe(b: &[u8]) -> Option<Info> {
    let (w, h, _) = root(text(b)?)?;
    let mut i = Info::new(
        Format::Svg,
        (w.max(1.0) + 0.5) as u32,
        (h.max(1.0) + 0.5) as u32,
    );
    i.alpha = true;
    Some(i)
}

/// Numbers in a list or path data: `1.5.5` is 1.5 and .5, `1-2` is 1 and -2, `1e-3` is one number.
fn numbers(s: &str) -> impl Iterator<Item = f32> + '_ {
    let b = s.as_bytes();
    let mut i = 0;
    core::iter::from_fn(move || {
        while i < b.len() && (b[i] == b',' || b[i].is_ascii_whitespace()) {
            i += 1;
        }
        let st = i;
        if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
            i += 1;
        }
        let mut dot = false;
        while i < b.len() && (b[i].is_ascii_digit() || (b[i] == b'.' && !dot)) {
            dot |= b[i] == b'.';
            i += 1;
        }
        if i < b.len()
            && (b[i] == b'e' || b[i] == b'E')
            && b.get(i + 1)
                .is_some_and(|c| c.is_ascii_digit() || *c == b'-' || *c == b'+')
        {
            i += 2;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i == st {
            return None;
        }
        s[st..i].parse::<f32>().ok()
    })
}

fn colour(v: &str) -> Option<Option<[u8; 3]>> {
    let v = v.trim();
    let hex = |s: &str| u8::from_str_radix(s, 16).ok();
    Some(Some(if let Some(h) = v.strip_prefix('#') {
        match h.len() {
            3 => {
                let d = |i: usize| hex(&h[i..i + 1]).map(|x| x * 17);
                [d(0)?, d(1)?, d(2)?]
            }
            6 => [hex(&h[0..2])?, hex(&h[2..4])?, hex(&h[4..6])?],
            _ => return None,
        }
    } else if let Some(r) = v.strip_prefix("rgb(").and_then(|r| r.strip_suffix(')')) {
        let mut it = r.split(',').map(|p| {
            let p = p.trim();
            match p.strip_suffix('%') {
                Some(q) => num(q).map(|x| (x * 2.55) as u8),
                None => num(p).map(|x| x.clamp(0.0, 255.0) as u8),
            }
        });
        [it.next()??, it.next()??, it.next()??]
    } else {
        match v {
            "none" | "transparent" => return Some(None),
            "black" | "currentColor" => [0, 0, 0],
            "white" => [255, 255, 255],
            "red" => [255, 0, 0],
            "green" => [0, 128, 0],
            "lime" => [0, 255, 0],
            "blue" => [0, 0, 255],
            "yellow" => [255, 255, 0],
            "orange" => [255, 165, 0],
            "purple" => [128, 0, 128],
            "gray" | "grey" => [128, 128, 128],
            "silver" => [192, 192, 192],
            "navy" => [0, 0, 128],
            "teal" => [0, 128, 128],
            _ => return None,
        }
    }))
}

#[derive(Clone, Copy)]
struct Style {
    fill: Option<[u8; 3]>,
    stroke: Option<[u8; 3]>,
    width: f32,
    opacity: f32,
    fill_op: f32,
    stroke_op: f32,
    even_odd: bool,
}

impl Style {
    fn set(&mut self, k: &str, v: &str) {
        match k {
            "fill" => {
                if let Some(c) = colour(v) {
                    self.fill = c;
                }
            }
            "stroke" => {
                if let Some(c) = colour(v) {
                    self.stroke = c;
                }
            }
            "stroke-width" => self.width = num(v).unwrap_or(self.width).max(0.0),
            "opacity" => self.opacity *= num(v).unwrap_or(1.0).clamp(0.0, 1.0),
            "fill-opacity" => self.fill_op = num(v).unwrap_or(1.0).clamp(0.0, 1.0),
            "stroke-opacity" => self.stroke_op = num(v).unwrap_or(1.0).clamp(0.0, 1.0),
            "fill-rule" => self.even_odd = v.trim() == "evenodd",
            _ => {}
        }
    }
}

fn transform(v: &str) -> Transform {
    let mut t = Transform::identity();
    for part in v.split(')') {
        let Some((name, args)) = part.split_once('(') else {
            continue;
        };
        let a: Vec<f32> = numbers(args).collect();
        let g = |i: usize, d: f32| a.get(i).copied().unwrap_or(d);
        let m = match name.trim().trim_start_matches(',').trim() {
            "translate" => Transform::from_translate(g(0, 0.0), g(1, 0.0)),
            "scale" => Transform::from_scale(g(0, 1.0), g(1, g(0, 1.0))),
            "rotate" if a.len() >= 3 => Transform::from_rotate_at(g(0, 0.0), g(1, 0.0), g(2, 0.0)),
            "rotate" => Transform::from_rotate(g(0, 0.0)),
            "matrix" if a.len() == 6 => Transform::from_row(a[0], a[1], a[2], a[3], a[4], a[5]),
            _ => continue,
        };
        t = t.pre_concat(m);
    }
    t
}

/// An elliptical arc as cubics (SVG implementation notes, F.6.5).
#[allow(clippy::too_many_arguments)]
fn arc(
    pb: &mut PathBuilder,
    x0: f32,
    y0: f32,
    rx: f32,
    ry: f32,
    rot: f32,
    large: bool,
    sweep: bool,
    x: f32,
    y: f32,
) {
    let (mut rx, mut ry) = (fabsf(rx), fabsf(ry));
    if rx == 0.0 || ry == 0.0 || (x0 == x && y0 == y) {
        pb.line_to(x, y);
        return;
    }
    let phi = rot.to_radians();
    let (s, c) = (sinf(phi), cosf(phi));
    let (dx, dy) = ((x0 - x) / 2.0, (y0 - y) / 2.0);
    let (x1, y1) = (c * dx + s * dy, -s * dx + c * dy);
    let l = x1 * x1 / (rx * rx) + y1 * y1 / (ry * ry);
    if l > 1.0 {
        rx *= sqrtf(l);
        ry *= sqrtf(l);
    }
    let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let mut k = sqrtf((num / den).max(0.0));
    if large == sweep {
        k = -k;
    }
    let (cx1, cy1) = (k * rx * y1 / ry, -k * ry * x1 / rx);
    let (cx, cy) = (
        c * cx1 - s * cy1 + (x0 + x) / 2.0,
        s * cx1 + c * cy1 + (y0 + y) / 2.0,
    );
    let ang = |ux: f32, uy: f32, vx: f32, vy: f32| atan2f(ux * vy - uy * vx, ux * vx + uy * vy);
    let t1 = ang(1.0, 0.0, (x1 - cx1) / rx, (y1 - cy1) / ry);
    let mut dt = ang(
        (x1 - cx1) / rx,
        (y1 - cy1) / ry,
        (-x1 - cx1) / rx,
        (-y1 - cy1) / ry,
    );
    let tau = core::f32::consts::TAU;
    if !sweep && dt > 0.0 {
        dt -= tau;
    } else if sweep && dt < 0.0 {
        dt += tau;
    }
    let n = libm::ceilf(fabsf(dt) / (tau / 4.0)).max(1.0) as usize;
    let step = dt / n as f32;
    let alpha = 4.0 / 3.0 * libm::tanf(step / 4.0);
    let pt = |t: f32| {
        let (ct, st) = (cosf(t), sinf(t));
        (
            cx + c * rx * ct - s * ry * st,
            cy + s * rx * ct + c * ry * st,
            -c * rx * st - s * ry * ct,
            -s * rx * st + c * ry * ct,
        )
    };
    let mut t = t1;
    for _ in 0..n {
        let (ax, ay, adx, ady) = pt(t);
        let (bx, by, bdx, bdy) = pt(t + step);
        pb.cubic_to(
            ax + alpha * adx,
            ay + alpha * ady,
            bx - alpha * bdx,
            by - alpha * bdy,
            bx,
            by,
        );
        t += step;
    }
}

fn path_data(d: &str) -> Option<Path> {
    let mut pb = PathBuilder::new();
    let b = d.as_bytes();
    let (mut x, mut y, mut sx, mut sy) = (0f32, 0f32, 0f32, 0f32);
    // the last control point, for S and T
    let (mut cx, mut cy) = (0f32, 0f32);
    let mut last = b' ';
    let mut i = 0;
    let mut cmd = b' ';
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_alphabetic() {
            cmd = c;
            i += 1;
        } else if c.is_ascii_whitespace() || c == b',' {
            i += 1;
            continue;
        } else if cmd == b' ' {
            return None;
        }
        // this command's numbers, up to the next letter
        let end = b[i..]
            .iter()
            .position(|c| c.is_ascii_alphabetic() && *c != b'e' && *c != b'E')
            .map_or(b.len(), |p| i + p);
        let nums: Vec<f32> = numbers(&d[i..end]).collect();
        i = end;
        let rel = cmd.is_ascii_lowercase();
        let n = match cmd.to_ascii_uppercase() {
            b'M' | b'L' | b'T' => 2,
            b'H' | b'V' => 1,
            b'C' => 6,
            b'S' | b'Q' => 4,
            b'A' => 7,
            b'Z' => 0,
            _ => return None,
        };
        if n == 0 {
            pb.close();
            x = sx;
            y = sy;
            last = b'Z';
            continue;
        }
        let mut first = true;
        for a in nums.chunks_exact(n) {
            let (ox, oy) = if rel { (x, y) } else { (0.0, 0.0) };
            let up = cmd.to_ascii_uppercase();
            match up {
                b'M' => {
                    x = a[0] + ox;
                    y = a[1] + oy;
                    if first {
                        pb.move_to(x, y);
                        sx = x;
                        sy = y;
                    } else {
                        pb.line_to(x, y);
                    }
                }
                b'L' => {
                    x = a[0] + ox;
                    y = a[1] + oy;
                    pb.line_to(x, y);
                }
                b'H' => {
                    x = a[0] + if rel { x } else { 0.0 };
                    pb.line_to(x, y);
                }
                b'V' => {
                    y = a[0] + if rel { y } else { 0.0 };
                    pb.line_to(x, y);
                }
                b'C' => {
                    pb.cubic_to(
                        a[0] + ox,
                        a[1] + oy,
                        a[2] + ox,
                        a[3] + oy,
                        a[4] + ox,
                        a[5] + oy,
                    );
                    cx = a[2] + ox;
                    cy = a[3] + oy;
                    x = a[4] + ox;
                    y = a[5] + oy;
                }
                b'S' => {
                    let (rx, ry) = if matches!(last, b'C' | b'S') {
                        (2.0 * x - cx, 2.0 * y - cy)
                    } else {
                        (x, y)
                    };
                    pb.cubic_to(rx, ry, a[0] + ox, a[1] + oy, a[2] + ox, a[3] + oy);
                    cx = a[0] + ox;
                    cy = a[1] + oy;
                    x = a[2] + ox;
                    y = a[3] + oy;
                }
                b'Q' => {
                    pb.quad_to(a[0] + ox, a[1] + oy, a[2] + ox, a[3] + oy);
                    cx = a[0] + ox;
                    cy = a[1] + oy;
                    x = a[2] + ox;
                    y = a[3] + oy;
                }
                b'T' => {
                    let (qx, qy) = if matches!(last, b'Q' | b'T') {
                        (2.0 * x - cx, 2.0 * y - cy)
                    } else {
                        (x, y)
                    };
                    x = a[0] + ox;
                    y = a[1] + oy;
                    pb.quad_to(qx, qy, x, y);
                    cx = qx;
                    cy = qy;
                }
                _ => {
                    let (ex, ey) = (a[5] + ox, a[6] + oy);
                    arc(
                        &mut pb,
                        x,
                        y,
                        a[0],
                        a[1],
                        a[2],
                        a[3] != 0.0,
                        a[4] != 0.0,
                        ex,
                        ey,
                    );
                    x = ex;
                    y = ey;
                }
            }
            last = up;
            first = false;
        }
    }
    pb.finish()
}

fn shape(name: &str, a: &[(&str, &str)]) -> Option<Path> {
    let get = |k: &str| a.iter().find(|(n, _)| *n == k).map(|(_, v)| *v);
    let f = |k: &str| get(k).and_then(num).unwrap_or(0.0);
    match name {
        "rect" => {
            let (x, y, w, h) = (f("x"), f("y"), f("width"), f("height"));
            let (mut rx, mut ry) = (get("rx").and_then(num), get("ry").and_then(num));
            if rx.is_none() {
                rx = ry;
            }
            if ry.is_none() {
                ry = rx;
            }
            let (rx, ry) = (
                rx.unwrap_or(0.0).min(w / 2.0),
                ry.unwrap_or(0.0).min(h / 2.0),
            );
            if rx <= 0.0 || ry <= 0.0 {
                return Some(PathBuilder::from_rect(Rect::from_xywh(x, y, w, h)?));
            }
            let k = 0.552_284_8;
            let mut pb = PathBuilder::new();
            pb.move_to(x + rx, y);
            pb.line_to(x + w - rx, y);
            pb.cubic_to(
                x + w - rx + k * rx,
                y,
                x + w,
                y + ry - k * ry,
                x + w,
                y + ry,
            );
            pb.line_to(x + w, y + h - ry);
            pb.cubic_to(
                x + w,
                y + h - ry + k * ry,
                x + w - rx + k * rx,
                y + h,
                x + w - rx,
                y + h,
            );
            pb.line_to(x + rx, y + h);
            pb.cubic_to(
                x + rx - k * rx,
                y + h,
                x,
                y + h - ry + k * ry,
                x,
                y + h - ry,
            );
            pb.line_to(x, y + ry);
            pb.cubic_to(x, y + ry - k * ry, x + rx - k * rx, y, x + rx, y);
            pb.close();
            pb.finish()
        }
        "circle" => PathBuilder::from_circle(f("cx"), f("cy"), f("r")),
        "ellipse" => {
            let (rx, ry) = (f("rx"), f("ry"));
            let mut pb = PathBuilder::new();
            pb.push_oval(Rect::from_xywh(
                f("cx") - rx,
                f("cy") - ry,
                2.0 * rx,
                2.0 * ry,
            )?);
            pb.finish()
        }
        "line" => {
            let mut pb = PathBuilder::new();
            pb.move_to(f("x1"), f("y1"));
            pb.line_to(f("x2"), f("y2"));
            pb.finish()
        }
        "polyline" | "polygon" => {
            let p: Vec<f32> = numbers(get("points")?).collect();
            let mut pb = PathBuilder::new();
            for (k, xy) in p.as_chunks::<2>().0.iter().enumerate() {
                if k == 0 {
                    pb.move_to(xy[0], xy[1]);
                } else {
                    pb.line_to(xy[0], xy[1]);
                }
            }
            if name == "polygon" {
                pb.close();
            }
            pb.finish()
        }
        "path" => path_data(get("d")?),
        _ => None,
    }
}

/// Draws the SVG with its long side `side` px.
pub fn render(b: &[u8], side: u32) -> Result<Pixels, Error> {
    let s = text(b).ok_or(Error::NotImage)?;
    let (w, h, vb) = root(s).ok_or(Error::NotImage)?;
    let scale = side as f32 / w.max(h).max(1.0);
    let (pw, ph) = (
        ((w * scale) as u32).clamp(1, side),
        ((h * scale) as u32).clamp(1, side),
    );
    let mut pm = Pixmap::new(pw, ph).ok_or(Error::TooBig)?;
    let base =
        Transform::from_scale(pw as f32 / vb[2], ph as f32 / vb[3]).pre_translate(-vb[0], -vb[1]);
    let none = Style {
        fill: Some([0, 0, 0]),
        stroke: None,
        width: 1.0,
        opacity: 1.0,
        fill_op: 1.0,
        stroke_op: 1.0,
        even_odd: false,
    };
    // (transform, style, hidden) for each open element
    let mut stack: Vec<(Transform, Style, bool)> = Vec::new();
    let mut name = "";
    let mut attrs: Vec<(&str, &str)> = Vec::new();
    let mut count = 0;
    for t in Tokenizer::from(s) {
        match t.map_err(|_| Error::Corrupt)? {
            Token::ElementStart { local, .. } => {
                name = local.as_str();
                attrs.clear();
                count += 1;
                if count > MAX_ELEMENTS {
                    break;
                }
            }
            Token::Attribute { local, value, .. } => attrs.push((local.as_str(), value.as_str())),
            Token::ElementEnd { end, .. } => {
                if let ElementEnd::Close(..) = end {
                    stack.pop();
                    continue;
                }
                let (pt, ps, hidden) = stack.last().copied().unwrap_or((base, none, false));
                let mut st = ps;
                let mut tr = pt;
                for &(k, v) in &attrs {
                    match k {
                        "transform" => tr = tr.pre_concat(transform(v)),
                        "style" => {
                            for decl in v.split(';') {
                                if let Some((k, v)) = decl.split_once(':') {
                                    st.set(k.trim(), v.trim());
                                }
                            }
                        }
                        _ => st.set(k, v),
                    }
                }
                let hidden = hidden
                    || matches!(
                        name,
                        "defs"
                            | "clipPath"
                            | "mask"
                            | "symbol"
                            | "pattern"
                            | "marker"
                            | "linearGradient"
                            | "radialGradient"
                            | "filter"
                            | "style"
                            | "title"
                            | "desc"
                            | "metadata"
                    )
                    || attrs
                        .iter()
                        .any(|(k, v)| *k == "display" && v.trim() == "none");
                if !hidden && let Some(p) = shape(name, &attrs) {
                    draw(&mut pm, &p, &st, tr);
                }
                if let ElementEnd::Open = end {
                    if stack.len() >= MAX_DEPTH {
                        return Err(Error::Corrupt);
                    }
                    stack.push((tr, st, hidden));
                }
            }
            _ => {}
        }
    }
    let mut px = Pixels::new(pw, ph, 4);
    for (o, p) in px
        .px
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(pm.data().as_chunks::<4>().0)
    {
        // premultiplied → straight
        let a = p[3] as u32;
        for c in 0..3 {
            o[c] = (p[c] as u32 * 255 + a / 2)
                .checked_div(a)
                .map_or(0, |v| v.min(255) as u8);
        }
        o[3] = p[3];
    }
    Ok(px)
}

fn draw(pm: &mut Pixmap, p: &Path, st: &Style, tr: Transform) {
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    if let Some([r, g, b]) = st.fill {
        paint.set_color_rgba8(r, g, b, (st.opacity * st.fill_op * 255.0) as u8);
        let rule = if st.even_odd {
            FillRule::EvenOdd
        } else {
            FillRule::Winding
        };
        pm.fill_path(p, &paint, rule, tr, None);
    }
    if let Some([r, g, b]) = st.stroke
        && st.width > 0.0
    {
        paint.set_color_rgba8(r, g, b, (st.opacity * st.stroke_op * 255.0) as u8);
        let stroke = Stroke {
            width: st.width,
            ..Stroke::default()
        };
        pm.stroke_path(p, &paint, &stroke, tr, None);
    }
}
