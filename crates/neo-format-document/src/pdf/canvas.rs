// SPDX-License-Identifier: Apache-2.0
//! hayro-interpret's [`Device`] drawn with tiny-skia: what a page's content stream draws (paths, glyph outlines,
//! images, shadings, tiling patterns) lands in a pixmap, through a stack of clip masks and transparency layers.
//! Ported from the logic of hayro's own renderer (`hayro/src`, which needs vello_cpu and std).

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell;
use hayro_interpret::encode::EncodedShadingType;
use hayro_interpret::font::{Glyph, GlyphRun};
use hayro_interpret::gradient::SvgGradientKind;
use hayro_interpret::kurbo::{self, Affine, BezPath, PathEl, Rect, Shape};
use hayro_interpret::pattern::{Pattern, TilingPattern};
use hayro_interpret::util::x_y_advances;
use hayro_interpret::{
    BlendMode, CacheKey, ClipPath, Device, DrawMode, DrawProps, FillRule, Image, ImageData,
    ImageDrawProps, LumaData, MaskType, Paint, SoftMask, StrokeProps,
};
use tiny_skia as sk;

/// At most this many transparency layers (each a whole page of RGBA) are alive; deeper groups draw straight into
/// the layer under them (a shortcut: their opacity and blend are lost, the page is marked partial).
const MAX_LAYERS: usize = 6;
/// A tiling pattern's cell or a shading's texture is at most this many pixels a side.
const MAX_TEXTURE: f64 = 4096.0;

/// One layer: the page, or an open transparency group.
struct Layer {
    pix: sk::Pixmap,
    opacity: f32,
    blend: sk::BlendMode,
    mask: Option<Rc<sk::Mask>>,
}

/// Draws a page (or a soft mask, a pattern cell) into a pixmap of `w` × `h`.
pub(crate) struct Canvas {
    w: u32,
    h: u32,
    layers: Vec<Layer>,
    /// One entry per open transparency group: whether it has a layer of its own (or draws straight through).
    groups: Vec<bool>,
    /// The clip stack: `None` = nothing clipped (the clip covers the whole canvas).
    clips: Vec<Option<Rc<sk::Mask>>>,
    /// Soft masks by their cache key, drawn once a page.
    soft: BTreeMap<u128, Rc<sk::Mask>>,
    /// Glyph outlines by glyph, converted once.
    glyphs: Rc<GlyphCache>,
    inside_pattern: bool,
    in_type3: bool,
    /// Something was left out (a layer over the limit, a texture too big).
    pub(crate) partial: Rc<Cell<bool>>,
}

#[derive(Default)]
pub(crate) struct GlyphCache(core::cell::RefCell<BTreeMap<u128, Option<Rc<sk::Path>>>>);

impl GlyphCache {
    /// At most this many outlines are kept (then the cache starts again).
    const MAX: usize = 4096;
}

impl Canvas {
    /// A canvas filled with `bg` (premultiplied RGBA).
    pub(crate) fn new(
        w: u32,
        h: u32,
        bg: sk::Color,
        glyphs: Rc<GlyphCache>,
        partial: Rc<Cell<bool>>,
    ) -> Option<Self> {
        let mut pix = sk::Pixmap::new(w, h)?;
        pix.fill(bg);
        Some(Canvas {
            w,
            h,
            layers: vec![Layer {
                pix,
                opacity: 1.0,
                blend: sk::BlendMode::SourceOver,
                mask: None,
            }],
            groups: Vec::new(),
            clips: Vec::new(),
            soft: BTreeMap::new(),
            glyphs,
            inside_pattern: false,
            in_type3: false,
            partial,
        })
    }

    /// The finished drawing (open layers, which a balanced page never leaves, are dropped).
    pub(crate) fn finish(mut self) -> sk::Pixmap {
        self.layers.truncate(1);
        self.layers
            .pop()
            .map(|l| l.pix)
            .unwrap_or_else(|| sk::Pixmap::new(1, 1).unwrap())
    }

    fn child(&self, w: u32, h: u32) -> Option<Canvas> {
        let mut c = Canvas::new(
            w,
            h,
            sk::Color::TRANSPARENT,
            self.glyphs.clone(),
            self.partial.clone(),
        )?;
        c.inside_pattern = self.inside_pattern;
        Some(c)
    }

    fn clip(&self) -> Option<&Rc<sk::Mask>> {
        self.clips.last().and_then(|c| c.as_ref())
    }

    fn target(&mut self) -> &mut sk::Pixmap {
        &mut self.layers.last_mut().unwrap().pix
    }

    fn misses(&self, bbox: Rect) -> bool {
        !bbox.overlaps(Rect::new(0.0, 0.0, self.w as f64, self.h as f64))
    }

    /// The clip mask with a soft mask multiplied in (a new mask), or the clip as it is.
    fn mask_with(&mut self, soft: Option<&SoftMask<'_>>) -> Option<Rc<sk::Mask>> {
        let clip = self.clip().cloned();
        let Some(soft) = soft else { return clip };
        let soft = self.soft_mask(soft)?;
        Some(match clip {
            None => soft,
            Some(clip) => {
                let mut m = (*soft).clone();
                for (a, b) in m.data_mut().iter_mut().zip(clip.data()) {
                    *a = mul(*a, *b);
                }
                Rc::new(m)
            }
        })
    }

    fn soft_mask(&mut self, m: &SoftMask<'_>) -> Option<Rc<sk::Mask>> {
        let key = m.cache_key();
        if let Some(found) = self.soft.get(&key) {
            return Some(found.clone());
        }
        let mut c = self.child(self.w, self.h)?;
        let bg = m.background_color().to_rgba().to_rgba8();
        if bg != [0, 0, 0, 255] {
            c.target()
                .fill(sk::Color::from_rgba8(bg[0], bg[1], bg[2], bg[3]));
        }
        m.interpret(&mut c);
        let pix = c.finish();
        let kind = match m.mask_type() {
            MaskType::Luminosity => sk::MaskType::Luminance,
            MaskType::Alpha => sk::MaskType::Alpha,
        };
        let mut mask = sk::Mask::from_pixmap(pix.as_ref(), kind);
        if let Some(f) = m.transfer_function() {
            f.apply_to(mask.data_mut());
        }
        let mask = Rc::new(mask);
        self.soft.insert(key, mask.clone());
        Some(mask)
    }

    fn push_clip_device(&mut self, path: &BezPath, rule: FillRule) {
        let next = match to_path(path, Affine::IDENTITY) {
            None => {
                // An empty clip hides everything.
                Some(Rc::new(sk::Mask::new(self.w, self.h).unwrap()))
            }
            Some(p) => {
                let whole = Rect::new(0.0, 0.0, self.w as f64, self.h as f64);
                if is_rect(path).is_some_and(|r| r.contains_rect(whole)) {
                    self.clip().cloned()
                } else {
                    let mut m = match self.clip() {
                        Some(m) => (**m).clone(),
                        None => {
                            let mut m = sk::Mask::new(self.w, self.h).unwrap();
                            m.data_mut().fill(255);
                            m
                        }
                    };
                    m.intersect_path(&p, fill_rule(rule), true, sk::Transform::identity());
                    Some(Rc::new(m))
                }
            }
        };
        self.clips.push(next);
    }

    /// Fills or strokes `path` (in user space, drawn with `transform`) with `paint`.
    fn paint_path(
        &mut self,
        path: &sk::Path,
        transform: Affine,
        paint: &Paint<'_>,
        props: (BlendMode, Option<&SoftMask<'_>>),
        how: How<'_>,
        bbox: Rect,
    ) {
        let device_bbox = transform.transform_rect_bbox(bbox);
        if self.misses(device_bbox.inflate(2.0, 2.0)) {
            return;
        }
        let mask = self.mask_with(props.1);
        let blend = blend(props.0);
        let ts = transform_of(transform);
        let partial = self.partial.clone();
        let clip_path = shading_clip(paint);
        let mut mask = mask;
        if let Some(cp) = clip_path.as_ref() {
            // A shading's /BBox clips its paint (in user space of the draw).
            let mut m = match mask.as_deref() {
                Some(m) => m.clone(),
                None => {
                    let mut m = sk::Mask::new(self.w, self.h).unwrap();
                    m.data_mut().fill(255);
                    m
                }
            };
            if let Some(p) = to_path(cp, Affine::IDENTITY) {
                m.intersect_path(&p, sk::FillRule::Winding, true, ts);
            }
            mask = Some(Rc::new(m));
        }
        let (w, h) = (self.w, self.h);
        let stroke_width = match how {
            How::Stroke(s) => s.width,
            How::Fill(_) => 0.0,
        };
        with_shader(
            self,
            paint,
            transform,
            device_bbox,
            stroke_width,
            (w, h),
            &partial,
            |canvas, shader| {
                let paint = sk::Paint {
                    shader,
                    blend_mode: blend,
                    anti_alias: true,
                    ..Default::default()
                };
                let target = canvas.target();
                match how {
                    How::Fill(rule) => target.fill_path(path, &paint, rule, ts, mask.as_deref()),
                    How::Stroke(s) => target.stroke_path(path, &paint, s, ts, mask.as_deref()),
                }
            },
        );
    }

    fn stroke_of(&self, s: &StrokeProps, transform: Affine, is_text: bool) -> sk::Stroke {
        // At least one device pixel wide, as the spec asks (hayro's rule; thinner for text and not in patterns).
        let threshold = if is_text { 0.25 } else { 1.0 };
        let (x, y) = x_y_advances(&transform);
        let factor = x.length().max(y.length()) as f32;
        let mut width = s.line_width.max(0.01 / factor.max(1e-6));
        let device = width * factor;
        if device < threshold && !self.inside_pattern && !self.in_type3 && device > 0.0 {
            width = width / device * threshold;
        }
        let dash = if s.dash_array.is_empty() {
            None
        } else {
            let mut d: Vec<f32> = s.dash_array.iter().copied().collect();
            if d.len() % 2 == 1 {
                d.extend_from_within(..);
            }
            sk::StrokeDash::new(d, s.dash_offset)
        };
        sk::Stroke {
            width,
            miter_limit: s.miter_limit,
            line_cap: match s.line_cap {
                kurbo::Cap::Butt => sk::LineCap::Butt,
                kurbo::Cap::Round => sk::LineCap::Round,
                kurbo::Cap::Square => sk::LineCap::Square,
            },
            line_join: match s.line_join {
                kurbo::Join::Bevel => sk::LineJoin::Bevel,
                kurbo::Join::Miter => sk::LineJoin::Miter,
                kurbo::Join::Round => sk::LineJoin::Round,
            },
            dash,
        }
    }

    fn draw_bez(&mut self, path: &BezPath, props: &DrawProps<'_>, mode: &DrawMode, is_text: bool) {
        let Some(p) = to_path(path, Affine::IDENTITY) else {
            return;
        };
        let bbox = path.bounding_box();
        let soft = (props.blend_mode, props.soft_mask.as_ref());
        match mode {
            DrawMode::Fill(rule) => self.paint_path(
                &p,
                props.transform,
                &props.paint,
                soft,
                How::Fill(fill_rule(*rule)),
                bbox,
            ),
            DrawMode::Stroke(s) => {
                let st = self.stroke_of(s, props.transform, is_text);
                let r = st.width as f64 * st.miter_limit.max(1.5) as f64;
                self.paint_path(
                    &p,
                    props.transform,
                    &props.paint,
                    soft,
                    How::Stroke(&st),
                    bbox.inflate(r, r),
                )
            }
            DrawMode::FillAndStroke(rule, s) => {
                self.paint_path(
                    &p,
                    props.transform,
                    &props.paint,
                    soft,
                    How::Fill(fill_rule(*rule)),
                    bbox,
                );
                let st = self.stroke_of(s, props.transform, is_text);
                let r = st.width as f64 * st.miter_limit.max(1.5) as f64;
                self.paint_path(
                    &p,
                    props.transform,
                    &props.paint,
                    soft,
                    How::Stroke(&st),
                    bbox.inflate(r, r),
                )
            }
            DrawMode::Invisible => {}
        }
    }

    fn outline(&self, glyph: &hayro_interpret::font::OutlineGlyph) -> Option<Rc<sk::Path>> {
        let key = glyph.identifier().cache_key();
        let mut cache = self.glyphs.0.borrow_mut();
        if let Some(found) = cache.get(&key) {
            return found.clone();
        }
        if cache.len() >= GlyphCache::MAX {
            cache.clear();
        }
        let path = to_path(&glyph.outline(), Affine::IDENTITY).map(Rc::new);
        cache.insert(key, path.clone());
        path
    }

    /// Draws an image (`w` × `h` premultiplied RGBA, its pixel space mapped by `transform`).
    fn draw_rgba(
        &mut self,
        rgba: sk::Pixmap,
        transform: Affine,
        props: &ImageDrawProps<'_>,
        smooth: bool,
    ) {
        let rect = Rect::new(0.0, 0.0, rgba.width() as f64, rgba.height() as f64);
        if self.misses(transform.transform_rect_bbox(rect)) {
            return;
        }
        let mask = self.mask_with(props.soft_mask.as_ref());
        let (x, y) = x_y_advances(&transform);
        // Shrinking or smooth images are filtered; enlarged ones keep their pixels (barcodes, scans).
        let shrinks = x.length() < 1.0 || y.length() < 1.0;
        let quality = if smooth || shrinks {
            sk::FilterQuality::Bilinear
        } else {
            sk::FilterQuality::Nearest
        };
        let shader = sk::Pattern::new(
            rgba.as_ref(),
            sk::SpreadMode::Pad,
            quality,
            1.0,
            sk::Transform::identity(),
        );
        let paint = sk::Paint {
            shader,
            blend_mode: blend(props.blend_mode),
            anti_alias: false,
            ..Default::default()
        };
        let r = sk::Rect::from_xywh(0.0, 0.0, rgba.width() as f32, rgba.height() as f32).unwrap();
        self.target()
            .fill_rect(r, &paint, transform_of(transform), mask.as_deref());
    }
}

enum How<'s> {
    Fill(sk::FillRule),
    Stroke(&'s sk::Stroke),
}

impl<'a> Device<'a> for Canvas {
    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, mode: &DrawMode) {
        self.draw_bez(path, &props, mode, false);
    }

    fn push_clip_path(&mut self, clip: &ClipPath) {
        self.push_clip_device(&clip.path, clip.fill);
    }

    fn push_clip_rect(&mut self, rect: &Rect) {
        self.push_clip_device(&rect.to_path(0.1), FillRule::NonZero);
    }

    fn push_transparency_group(
        &mut self,
        opacity: f32,
        mask: Option<SoftMask<'a>>,
        mode: BlendMode,
    ) {
        let plain = opacity >= 1.0 && mask.is_none() && matches!(mode, BlendMode::Normal);
        let pix = if plain || self.layers.len() > MAX_LAYERS {
            None
        } else {
            sk::Pixmap::new(self.w, self.h)
        };
        let Some(pix) = pix else {
            // Drawn straight into the layer under it: exact for an opaque normal group, an approximation otherwise.
            if !plain {
                self.partial.set(true);
            }
            self.groups.push(false);
            return;
        };
        let mask = mask.and_then(|m| self.soft_mask(&m));
        self.layers.push(Layer {
            pix,
            opacity,
            blend: blend(mode),
            mask,
        });
        self.groups.push(true);
    }

    fn pop_transparency_group(&mut self) {
        if self.groups.pop() != Some(true) || self.layers.len() < 2 {
            return;
        }
        let top = self.layers.pop().unwrap();
        let mut mask = self.clip().cloned();
        if let Some(soft) = top.mask {
            mask = Some(match mask {
                None => soft,
                Some(clip) => {
                    let mut m = (*soft).clone();
                    for (a, b) in m.data_mut().iter_mut().zip(clip.data()) {
                        *a = mul(*a, *b);
                    }
                    Rc::new(m)
                }
            });
        }
        let paint = sk::PixmapPaint {
            opacity: top.opacity,
            blend_mode: top.blend,
            quality: sk::FilterQuality::Nearest,
        };
        self.target().draw_pixmap(
            0,
            0,
            top.pix.as_ref(),
            &paint,
            sk::Transform::identity(),
            mask.as_deref(),
        );
    }

    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, mode: &DrawMode) {
        for glyph in run.glyphs() {
            let t = props.transform * glyph.transform();
            match &**glyph {
                Glyph::Outline(g) => {
                    let Some(path) = self.outline(g) else {
                        continue;
                    };
                    let b = path.bounds();
                    let bbox = Rect::new(
                        b.left() as f64,
                        b.top() as f64,
                        b.right() as f64,
                        b.bottom() as f64,
                    );
                    let soft = (props.blend_mode, props.soft_mask.as_ref());
                    match mode {
                        DrawMode::Fill(_) => self.paint_path(
                            &path,
                            t,
                            &props.paint,
                            soft,
                            How::Fill(sk::FillRule::Winding),
                            bbox,
                        ),
                        DrawMode::Stroke(s) => {
                            let st = self.stroke_of(s, t, true);
                            self.paint_path(
                                &path,
                                t,
                                &props.paint,
                                soft,
                                How::Stroke(&st),
                                bbox.inflate(50.0, 50.0),
                            )
                        }
                        DrawMode::FillAndStroke(_, s) => {
                            self.paint_path(
                                &path,
                                t,
                                &props.paint,
                                soft,
                                How::Fill(sk::FillRule::Winding),
                                bbox,
                            );
                            let st = self.stroke_of(s, t, true);
                            self.paint_path(
                                &path,
                                t,
                                &props.paint,
                                soft,
                                How::Stroke(&st),
                                bbox.inflate(50.0, 50.0),
                            )
                        }
                        DrawMode::Invisible => {}
                    }
                }
                Glyph::Type3(g) => {
                    if matches!(mode, DrawMode::Invisible) {
                        continue;
                    }
                    self.in_type3 = true;
                    g.interpret(self, props.transform, glyph.transform(), &props.paint);
                    self.in_type3 = false;
                }
            }
        }
    }

    fn draw_image(&mut self, image: Image<'a, '_>, props: ImageDrawProps<'a>) {
        let (x, y) = x_y_advances(&props.transform);
        let target = (
            (ceil(x.length() * image.width() as f64) as u32).clamp(1, 8192),
            (ceil(y.length() * image.height() as f64) as u32).clamp(1, 8192),
        );
        match image {
            Image::Raster(r) => r.with_rgba(
                |data, alpha| {
                    let (sx, sy) = data.scale_factors();
                    let smooth = data.interpolate();
                    if let Some(rgba) = rgba_of(&data, alpha.as_ref()) {
                        let t = props.transform * Affine::scale_non_uniform(sx as f64, sy as f64);
                        self.draw_rgba(rgba, t, &props, smooth);
                    }
                },
                Some(target),
            ),
            Image::Stencil(s) => s.with_stencil(
                |stencil, paint| {
                    let t = props.transform
                        * Affine::scale_non_uniform(
                            stencil.scale_factors.0 as f64,
                            stencil.scale_factors.1 as f64,
                        );
                    match paint {
                        Paint::Color(c) => {
                            let c = c.to_rgba().to_rgba8();
                            if let Some(rgba) = stencil_rgba(&stencil, c) {
                                self.draw_rgba(rgba, t, &props, stencil.interpolate);
                            }
                        }
                        Paint::Pattern(_) => {
                            // The stencil as a mask over the whole canvas, filled with the pattern.
                            let Some(white) = stencil_rgba(&stencil, [255, 255, 255, 255]) else {
                                return;
                            };
                            let Some(mut c) = self.child(self.w, self.h) else {
                                return;
                            };
                            c.draw_rgba(
                                white,
                                t,
                                &ImageDrawProps {
                                    soft_mask: None,
                                    ..props.clone()
                                },
                                true,
                            );
                            let mut m =
                                sk::Mask::from_pixmap(c.finish().as_ref(), sk::MaskType::Alpha);
                            if let Some(clip) = self.clip() {
                                for (a, b) in m.data_mut().iter_mut().zip(clip.data()) {
                                    *a = mul(*a, *b);
                                }
                            }
                            let whole = Rect::new(0.0, 0.0, self.w as f64, self.h as f64);
                            let rect = whole.to_path(0.1);
                            if let Some(p) = to_path(&rect, Affine::IDENTITY) {
                                self.clips.push(Some(Rc::new(m)));
                                self.paint_path(
                                    &p,
                                    Affine::IDENTITY,
                                    paint,
                                    (props.blend_mode, None),
                                    How::Fill(sk::FillRule::Winding),
                                    whole,
                                );
                                self.clips.pop();
                            }
                        }
                    }
                },
                Some(target),
            ),
        }
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }
}

/// Calls `draw` with a tiny-skia shader for `paint` (a colour, a gradient, a sampled shading or a tiling pattern's
/// cell; the pixmaps behind them live for the call).
#[allow(clippy::too_many_arguments)]
fn with_shader(
    canvas: &mut Canvas,
    paint: &Paint<'_>,
    path_transform: Affine,
    device_bbox: Rect,
    stroke_width: f32,
    (w, h): (u32, u32),
    partial: &Rc<Cell<bool>>,
    draw: impl FnOnce(&mut Canvas, sk::Shader<'_>),
) {
    let to_user = path_transform.inverse();
    match paint {
        Paint::Color(c) => {
            let c = c.to_rgba().to_rgba8();
            draw(
                canvas,
                sk::Shader::SolidColor(sk::Color::from_rgba8(c[0], c[1], c[2], c[3])),
            );
        }
        Paint::Pattern(p) => {
            let mut bbox = device_bbox;
            if stroke_width > 0.0 {
                let (a, b) = x_y_advances(&path_transform);
                let r = a.length().max(b.length()) * stroke_width as f64;
                bbox = bbox.inflate(r, r);
            }
            let bbox = bbox.intersect(Rect::new(0.0, 0.0, w as f64, h as f64));
            if bbox.width() <= 0.0 || bbox.height() <= 0.0 {
                return;
            }
            match &**p {
                Pattern::Shading(s) => {
                    let encoded = s.encode();
                    if let EncodedShadingType::RadialAxial(g) = &encoded.shading_type
                        && let Some(native) = g.as_svg_gradient(&encoded, bbox, 0.01)
                    {
                        let stops: Vec<sk::GradientStop> = native
                            .stops
                            .iter()
                            .map(|s| {
                                let [r, g, b, a] = s.color;
                                sk::GradientStop::new(
                                    s.offset,
                                    sk::Color::from_rgba(r, g, b, a).unwrap_or(sk::Color::BLACK),
                                )
                            })
                            .collect();
                        let t = transform_of(to_user * native.transform);
                        let shader = match native.kind {
                            SvgGradientKind::Linear { start, end } => sk::LinearGradient::new(
                                pt(start),
                                pt(end),
                                stops,
                                sk::SpreadMode::Pad,
                                t,
                            ),
                            SvgGradientKind::Radial {
                                start_center,
                                start_radius,
                                end_center,
                                end_radius,
                            } => sk::RadialGradient::new(
                                pt(start_center),
                                start_radius,
                                pt(end_center),
                                end_radius,
                                stops,
                                sk::SpreadMode::Pad,
                                t,
                            ),
                        };
                        if let Some(shader) = shader {
                            draw(canvas, shader);
                        }
                        return;
                    }
                    if bbox.width() > MAX_TEXTURE || bbox.height() > MAX_TEXTURE {
                        partial.set(true);
                        return;
                    }
                    let mut px =
                        Vec::with_capacity((ceil(bbox.width()) * ceil(bbox.height())) as usize * 4);
                    let (tw, th, t) = encoded.sample_texture(bbox, 1.0, |[r, g, b, a]| {
                        let c = sk::Color::from_rgba(
                            r.clamp(0.0, 1.0),
                            g.clamp(0.0, 1.0),
                            b.clamp(0.0, 1.0),
                            a.clamp(0.0, 1.0),
                        )
                        .unwrap_or(sk::Color::TRANSPARENT)
                        .premultiply()
                        .to_color_u8();
                        px.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
                    });
                    let Some(tex) =
                        sk::IntSize::from_wh(tw, th).and_then(|s| sk::Pixmap::from_vec(px, s))
                    else {
                        return;
                    };
                    let shader = sk::Pattern::new(
                        tex.as_ref(),
                        sk::SpreadMode::Pad,
                        sk::FilterQuality::Bilinear,
                        1.0,
                        transform_of(to_user * t),
                    );
                    draw(canvas, shader);
                }
                Pattern::Tiling(t) => {
                    let Some((tex, t_tex)) = tiling(canvas, t, bbox, stroke_width > 0.0) else {
                        partial.set(true);
                        return;
                    };
                    let shader = sk::Pattern::new(
                        tex.as_ref(),
                        sk::SpreadMode::Repeat,
                        sk::FilterQuality::Bilinear,
                        t.opacity,
                        transform_of(to_user * t_tex),
                    );
                    draw(canvas, shader);
                }
            }
        }
    }
}

/// A tiling pattern's cell (or the part of it over `area`, when it doesn't repeat there) drawn into a pixmap, and
/// the transform from the pixmap to the device (hayro's `tiling.rs`).
fn tiling(
    canvas: &Canvas,
    t: &TilingPattern<'_>,
    area: Rect,
    is_stroke: bool,
) -> Option<(sk::Pixmap, Affine)> {
    let area = t.matrix.inverse().transform_rect_bbox(area);
    let (xa, ya) = x_y_advances(&t.matrix);
    let x = Axis::new(
        xa.length(),
        t.bbox.x0..t.bbox.x1,
        t.x_step,
        area.x0..area.x1,
    )?;
    let y = Axis::new(
        ya.length(),
        t.bbox.y0..t.bbox.y1,
        t.y_step,
        area.y0..area.y1,
    )?;
    let mut c = canvas.child(x.size, y.size)?;
    c.inside_pattern = true;
    let to_texture =
        Affine::scale_non_uniform(x.scale, y.scale) * Affine::translate((-x.start, -y.start));
    let copies = (x.copies.end() - x.copies.start() + 1) as i64
        * (y.copies.end() - y.copies.start() + 1) as i64;
    if copies > 4096 {
        return None;
    }
    for j in y.copies.clone() {
        for i in x.copies.clone() {
            let offset = Affine::translate((f64::from(i) * x.step, f64::from(j) * y.step));
            t.interpret(&mut c, to_texture * offset, is_stroke);
        }
    }
    let transform = t.matrix
        * Affine::translate((x.start, y.start))
        * Affine::scale_non_uniform(1.0 / x.scale, 1.0 / y.scale);
    Some((c.finish(), transform))
}

struct Axis {
    start: f64,
    scale: f64,
    size: u32,
    step: f64,
    copies: core::ops::RangeInclusive<i32>,
}

impl Axis {
    fn new(
        scale: f64,
        cell: core::ops::Range<f64>,
        step: f32,
        area: core::ops::Range<f64>,
    ) -> Option<Axis> {
        let step = f64::from(step.abs());
        if !(step > 0.0 && step.is_finite() && scale.is_finite()) {
            return None;
        }
        let width = cell.end - cell.start;
        let visible = (area.end - area.start).max(0.0);
        let scale = round(step * scale).max(1.0) / step;
        let axis = if visible >= step {
            Axis {
                start: cell.start,
                scale,
                size: round(step * scale) as u32,
                step,
                copies: floor(-width / step) as i32 + 1..=0,
            }
        } else {
            let start = cell.start + floor((area.start - cell.start) * scale) / scale;
            Axis {
                start,
                scale,
                size: ceil((area.end - start) * scale).max(1.0) as u32,
                step,
                copies: ceil((area.start - cell.end) / step) as i32
                    ..=floor((area.end - cell.start) / step) as i32,
            }
        };
        (axis.size as f64 <= MAX_TEXTURE && axis.size > 0).then_some(axis)
    }
}

fn round(x: f64) -> f64 {
    libm::round(x)
}
fn floor(x: f64) -> f64 {
    libm::floor(x)
}
fn ceil(x: f64) -> f64 {
    libm::ceil(x)
}

/// The paint's shading clip (a shading pattern's `/BBox`), if any.
fn shading_clip(paint: &Paint<'_>) -> Option<BezPath> {
    match paint {
        Paint::Pattern(p) => match &**p {
            Pattern::Shading(s) => s.shading.clip_path.clone(),
            Pattern::Tiling(_) => None,
        },
        Paint::Color(_) => None,
    }
}

/// An image as premultiplied RGBA, its alpha (of any size) scaled onto it.
fn rgba_of(data: &ImageData, alpha: Option<&LumaData>) -> Option<sk::Pixmap> {
    let (w, h) = (data.width(), data.height());
    let size = sk::IntSize::from_wh(w, h)?;
    let n = w as usize * h as usize;
    let mut px = Vec::new();
    px.try_reserve_exact(n * 4).ok()?;
    match data {
        ImageData::Rgb(d) => {
            for c in d.data.as_chunks::<3>().0.iter().take(n) {
                px.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        ImageData::Luma(d) => {
            for &l in d.data.iter().take(n) {
                px.extend_from_slice(&[l, l, l, 255]);
            }
        }
    }
    px.resize(n * 4, 255);
    if let Some(a) = alpha {
        let (aw, ah) = (a.width.max(1) as usize, a.height.max(1) as usize);
        for y in 0..h as usize {
            let ay = y * ah / h as usize;
            for x in 0..w as usize {
                let ax = x * aw / w as usize;
                let v = a.data.get(ay * aw + ax).copied().unwrap_or(255);
                let p = &mut px[(y * w as usize + x) * 4..][..4];
                p[0] = mul(p[0], v);
                p[1] = mul(p[1], v);
                p[2] = mul(p[2], v);
                p[3] = v;
            }
        }
    }
    sk::Pixmap::from_vec(px, size)
}

/// A stencil (1 = paint) in a colour, premultiplied.
fn stencil_rgba(s: &LumaData, c: [u8; 4]) -> Option<sk::Pixmap> {
    let size = sk::IntSize::from_wh(s.width, s.height)?;
    let n = s.width as usize * s.height as usize;
    let mut px = Vec::new();
    px.try_reserve_exact(n * 4).ok()?;
    for i in 0..n {
        let a = mul(s.data.get(i).copied().unwrap_or(0), c[3]);
        px.extend_from_slice(&[mul(c[0], a), mul(c[1], a), mul(c[2], a), a]);
    }
    sk::Pixmap::from_vec(px, size)
}

fn mul(a: u8, b: u8) -> u8 {
    ((u16::from(a) * u16::from(b) + 127) / 255) as u8
}

fn pt(p: kurbo::Point) -> sk::Point {
    sk::Point::from_xy(p.x as f32, p.y as f32)
}

pub(crate) fn transform_of(a: Affine) -> sk::Transform {
    let [sx, ky, kx, sy, tx, ty] = a.as_coeffs();
    sk::Transform::from_row(
        sx as f32, ky as f32, kx as f32, sy as f32, tx as f32, ty as f32,
    )
}

fn fill_rule(r: FillRule) -> sk::FillRule {
    match r {
        FillRule::NonZero => sk::FillRule::Winding,
        FillRule::EvenOdd => sk::FillRule::EvenOdd,
    }
}

fn blend(m: BlendMode) -> sk::BlendMode {
    match m {
        BlendMode::Normal => sk::BlendMode::SourceOver,
        BlendMode::Multiply => sk::BlendMode::Multiply,
        BlendMode::Screen => sk::BlendMode::Screen,
        BlendMode::Overlay => sk::BlendMode::Overlay,
        BlendMode::Darken => sk::BlendMode::Darken,
        BlendMode::Lighten => sk::BlendMode::Lighten,
        BlendMode::ColorDodge => sk::BlendMode::ColorDodge,
        BlendMode::ColorBurn => sk::BlendMode::ColorBurn,
        BlendMode::HardLight => sk::BlendMode::HardLight,
        BlendMode::SoftLight => sk::BlendMode::SoftLight,
        BlendMode::Difference => sk::BlendMode::Difference,
        BlendMode::Exclusion => sk::BlendMode::Exclusion,
        BlendMode::Hue => sk::BlendMode::Hue,
        BlendMode::Saturation => sk::BlendMode::Saturation,
        BlendMode::Color => sk::BlendMode::Color,
        BlendMode::Luminosity => sk::BlendMode::Luminosity,
    }
}

/// A kurbo path as a tiny-skia path (`None` when empty or not finite).
pub(crate) fn to_path(path: &BezPath, t: Affine) -> Option<sk::Path> {
    let mut b = sk::PathBuilder::new();
    let p = |q: kurbo::Point| {
        let q = t * q;
        (q.x as f32, q.y as f32)
    };
    for el in path.elements() {
        match *el {
            PathEl::MoveTo(a) => {
                let (x, y) = p(a);
                b.move_to(x, y)
            }
            PathEl::LineTo(a) => {
                let (x, y) = p(a);
                b.line_to(x, y)
            }
            PathEl::QuadTo(a, c) => {
                let ((x1, y1), (x, y)) = (p(a), p(c));
                b.quad_to(x1, y1, x, y)
            }
            PathEl::CurveTo(a, c, d) => {
                let ((x1, y1), (x2, y2), (x, y)) = (p(a), p(c), p(d));
                b.cubic_to(x1, y1, x2, y2, x, y)
            }
            PathEl::ClosePath => b.close(),
        }
    }
    b.finish()
}

/// The rectangle a path is, if it is one (axis-aligned, four corners).
fn is_rect(path: &BezPath) -> Option<Rect> {
    let els = path.elements();
    let pts: Vec<kurbo::Point> = els
        .iter()
        .filter_map(|e| match e {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => Some(*p),
            PathEl::ClosePath => None,
            _ => Some(kurbo::Point::new(f64::NAN, f64::NAN)),
        })
        .collect();
    let pts = match pts.as_slice() {
        [a, b, c, d, e] if a == e => [*a, *b, *c, *d],
        [a, b, c, d] => [*a, *b, *c, *d],
        _ => return None,
    };
    let r = Rect::from_points(pts[0], pts[2]);
    let on_edge = |p: kurbo::Point| (p.x == r.x0 || p.x == r.x1) && (p.y == r.y0 || p.y == r.y1);
    let axis = |a: kurbo::Point, b: kurbo::Point| a.x == b.x || a.y == b.y;
    (pts.iter().all(|p| on_edge(*p)) && (0..4).all(|i| axis(pts[i], pts[(i + 1) % 4]))).then_some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_draws() {
        let mut c = Canvas::new(100, 100, sk::Color::WHITE, Rc::default(), Rc::default()).unwrap();
        let mut img = sk::Pixmap::new(10, 5).unwrap();
        img.fill(sk::Color::from_rgba8(255, 0, 0, 255));
        let props = ImageDrawProps {
            transform: Affine::IDENTITY,
            soft_mask: None,
            blend_mode: BlendMode::Normal,
        };
        let t = Affine::new([5.0, 0.0, 0.0, -10.0, 10.0, 90.0]);
        c.draw_rgba(img, t, &props, false);
        let pix = c.finish();
        let p = pix.pixel(30, 70).unwrap();
        assert_eq!((p.red(), p.green(), p.blue()), (255, 0, 0));
    }
}
