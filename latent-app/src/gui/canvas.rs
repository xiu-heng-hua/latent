//! The central canvas: the output transform that turns a rendered image into an
//! egui texture, and the `CentralPanel` that shows the photo (wrapped in the
//! neutral gray surround) and paints brush dabs onto a brush mask.

use eframe::egui;
use latent_edit::{Dab, MaskShape};
use latent_image::ImageBuf;

use super::app::App;
use super::theme;

/// Convert a linear working-RGB image to a gamma-encoded egui texture, using the
/// exact output transform export uses ([`latent_export::to_srgb8`] — working→sRGB
/// matrix, highlight rolloff, sRGB OETF) so the preview matches the saved file.
pub(crate) fn to_color_image(img: &ImageBuf) -> egui::ColorImage {
    let bytes = latent_export::to_srgb8(img);
    egui::ColorImage::from_rgb([img.width() as usize, img.height() as usize], &bytes)
}

/// Show the central canvas. Until the first preview texture arrives, paints a
/// placeholder; once it's ready, draws the photo on the neutral surround and
/// handles brush painting (one undo step per stroke). The surround changes only
/// the area *around* the photo — the `Image` still draws `to_color_image`'s
/// bytes unaltered.
pub(crate) fn show(app: &mut App, ctx: &egui::Context) {
    let surround = egui::Frame::central_panel(&ctx.style()).fill(theme::CANVAS_SURROUND);

    // The preview renders off-thread, so the texture is not ready on the first
    // frame(s). Until it arrives, show a placeholder rather than unwrapping a
    // `None` texture, and keep waiting for the worker.
    let Some(texture) = &app.texture else {
        egui::CentralPanel::default()
            .frame(surround)
            .show(ctx, |ui| {
                ui.centered_and_justified(|ui| ui.label("Rendering…"));
            });
        ctx.request_repaint();
        return;
    };
    let tex_id = texture.id();
    let tex_size = texture.size_vec2();
    let active = app.active;
    let local_sel = app.local_sel;
    let mut painted = false;
    egui::CentralPanel::default()
        .frame(surround)
        .show(ctx, |ui| {
            egui::ScrollArea::both().show(ui, |ui| {
                let resp = ui.add(
                    egui::Image::new(egui::load::SizedTexture::new(tex_id, tex_size))
                        .sense(egui::Sense::click_and_drag()),
                );
                // Paint brush dabs when the selected local is a brush mask, one
                // undo step per stroke (begin on press, commit on release).
                let is_brush = app.variants[active]
                    .current()
                    .locals
                    .get(local_sel)
                    .is_some_and(|l| matches!(l.mask.shapes.first(), Some(MaskShape::Brush(_))));
                if is_brush {
                    let click = resp.clicked() && !resp.dragged();
                    if resp.drag_started() || click {
                        app.variants[active].begin();
                    }
                    if (resp.dragged() || click)
                        && let Some(pos) = resp.hover_pos()
                    {
                        let r = resp.rect;
                        let nx = ((pos.x - r.left()) / r.width().max(1.0)).clamp(0.0, 1.0);
                        let ny = ((pos.y - r.top()) / r.height().max(1.0)).clamp(0.0, 1.0);
                        if let Some(MaskShape::Brush(b)) = app.variants[active].current_mut().locals
                            [local_sel]
                            .mask
                            .shapes
                            .first_mut()
                        {
                            b.dabs.push(Dab {
                                x: nx,
                                y: ny,
                                radius: app.brush_radius,
                                feather: app.brush_feather,
                                erase: app.brush_erase,
                            });
                            painted = true;
                        }
                    }
                    if resp.drag_stopped() || click {
                        app.variants[active].commit();
                    }
                }
            });
        });
    // A painted stroke changed the settings after this frame's render; refresh
    // the preview and repaint so the dab shows up.
    if painted {
        app.render_preview(ctx);
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_color_image_matches_the_export_transform() {
        // The preview must go through the same output transform as a saved file
        // (working→sRGB matrix + highlight rolloff + sRGB OETF). Neutrals stay
        // neutral, so the values match the export tests: 0.5 → 188, and display
        // white 1.0 rolls off to 254 (not a bare 255).
        let mut img = ImageBuf::new(3, 1);
        img.set(0, 0, [0.0, 0.0, 0.0]); // black
        img.set(1, 0, [0.5, 0.5, 0.5]); // mid-gray (below the knee, faithful)
        img.set(2, 0, [1.0, 1.0, 1.0]); // display white (rolled off)

        let ci = to_color_image(&img);
        assert_eq!(ci.size, [3, 1]);
        assert_eq!(ci.pixels[0], egui::Color32::from_rgb(0, 0, 0));
        assert_eq!(ci.pixels[1], egui::Color32::from_rgb(188, 188, 188));
        assert_eq!(ci.pixels[2], egui::Color32::from_rgb(254, 254, 254));
    }
}
