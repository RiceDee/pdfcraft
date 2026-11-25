//! Scan & OCR ▸ Enhance scanned file in the real shell (egui_kittest): the dialog, the run and
//! its report, that the page keeps its comment while its scan is cleaned in place, and the result
//! window whose sliders tune restored halftone scans.

use std::sync::Arc;

use crate::support::AppHarness as _;
use craft::host::ManualJobs;
use craft::pages::ui::PageRangeOptions;
use craft::pages::{PageRange, PageScope};
use craft::ui::dialog::dialog_id;
use craft::ui::patterns::form_dialogs::HostedParamDialog;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use pdfcraft_engine::{Edit, NewAnnotation, NoteIcon, Session, Shape, Style};
use pdfcraft_render::{PageRenderer, RenderRequest};
use pdfcraft_ui_egui::PdfCraftApp;

/// A two-page PDF: page 1 a grey scan with a darker gutter and text bars (and a sticky note),
/// page 2 the same scan again.
fn scan() -> Vec<u8> {
    let img = image::GrayImage::from_fn(300, 400, |x, y| {
        let light = 0.55 + 0.45 * (x as f32 / 60.0).min(1.0);
        let ink = if (60..360).contains(&y) && (y - 60) % 40 < 6 && (60..240).contains(&x) { 0.12 } else { 0.78 };
        image::Luma([(ink * light * 255.0) as u8])
    });
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let mut s = Session::new();
    let bytes = s.create_from_images(&[("a.png".into(), png.clone()), ("b.png".into(), png)]).unwrap();
    let id = s.open("scan.pdf", None, bytes, None).unwrap();
    let shape = Shape::Note { at: [20.0, 20.0], icon: NoteIcon::Comment };
    let note = NewAnnotation { page: 0, style: Style::default_for(&shape), shape, contents: "Keep me".into(), author: "Tester".into() };
    s.apply(id, Edit::AddAnnotation(note)).unwrap();
    s.save_bytes(id).unwrap().to_vec()
}

/// The scan open in an app whose jobs wait until `run_jobs` runs them.
fn harness() -> (Harness<'static, PdfCraftApp>, Arc<ManualJobs>) {
    let (app, jobs) = crate::control::app_with_manual_jobs();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |cc| {
        crate::support::install_runtime(&cc.egui_ctx);
        {
            let mut app = app;
            app.set_option(&egui::Context::default(), "language", "en").unwrap();
            app.open_bytes(&egui::Context::default(), "scan.pdf", None, scan()).unwrap();
            app
        }
    });
    h.run_steps(4);
    (h, jobs)
}

/// Runs the host's queued jobs, then lets the app poll them.
fn run_jobs(h: &mut Harness<'static, PdfCraftApp>, jobs: &ManualJobs) {
    jobs.run_all();
    h.run_steps(3);
}

#[test]
fn enhance_scanned_file_cleans_the_chosen_pages_and_says_what_it_did() {
    let (mut h, jobs) = harness();
    let id = h.state().active_ids().unwrap().1;
    assert!(h.state().session.get(id).unwrap().comment_props(0, 0).is_some());
    assert!(h.execute("ocr.enhance"));
    h.run_steps(2);
    let ctx = h.ctx.clone();
    let current = PageRange { scope: PageScope::Current, ..PageRange::default() };
    let value = serde_json::to_value(PageRangeOptions::field_value(&current)).unwrap();
    h.state_mut().dialogs.set_json_field(&ctx, dialog_id("enhance-scan"), "pages", value).unwrap();
    h.run_steps(1);
    assert!(h.state_mut().dialogs.confirm::<HostedParamDialog>(&ctx));
    h.run_steps(2);
    run_jobs(&mut h, &jobs);
    assert!(crate::support::shown_notice(&h).is_some(), "a notice says what was done");
    let app = h.state();
    assert!(app.scan_clean_run.is_none() && !app.dialogs.is_open::<HostedParamDialog>(), "finished and closed");
    let doc = app.session.get(id).unwrap();
    assert!(doc.can_undo().is_some(), "one undo step restores the page");
    assert_eq!(doc.info.pages.len(), 2);
    assert_eq!(doc.comment_props(0, 0).map(|p| p.author), Some("Tester".to_string()), "the sticky note stays");
}

#[test]
fn nothing_to_enhance_is_said_not_done() {
    let (mut h, jobs) = harness();
    let id = h.state().active_ids().unwrap().1;
    h.state_mut().scan_clean_draft.settings = pdfcraft_engine::scan_clean::ScanCleanSettings {
        level: 0,
        edge_shadows: false,
        despeckle: false,
        restore_halftones: false,
        ..Default::default()
    };
    // With every filter off the dialog's OK is disabled; starting anyway changes nothing.
    let ctx_h = h.ctx.clone();
    h.state_mut().start_scan_clean(&ctx_h);
    run_jobs(&mut h, &jobs);
    assert!(crate::support::shown_notice(&h).is_some(), "a notice says nothing was cleaned");
    let app = h.state();
    assert_eq!(app.session.get(id).unwrap().can_undo(), None);
}

/// A PDF of 96 x 96 pages, each a halftone scan (a one-bit picture of its own: 20 % dots as paper,
/// a block of 70 % dots as ink, uncompressed) or, where `scans` says false, an empty page.
fn halftone(scans: &[bool]) -> Vec<u8> {
    let mut bits = vec![0u8; 12 * 96];
    for (y, x) in (0..96usize).flat_map(|y| (0..96usize).map(move |x| (y, x))) {
        let ink = (30..66).contains(&x) && (30..66).contains(&y);
        if (x * 7 + y * 3) % 10 >= if ink { 7 } else { 2 } {
            bits[y * 12 + x / 8] |= 0x80 >> (x % 8);
        }
    }
    let n = scans.len();
    let kids: String = (0..n).map(|i| format!("{} 0 R ", 3 + 3 * i)).collect();
    let mut pdf =
        format!("%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [{kids}] /Count {n} >> endobj\n")
            .into_bytes();
    for (i, scan) in scans.iter().enumerate() {
        let (page, image, contents) = (3 + 3 * i, 4 + 3 * i, 5 + 3 * i);
        let draw = if *scan { "q 96 0 0 96 0 0 cm /Im0 Do Q" } else { "q Q" };
        pdf.extend(
            format!(
                "{page} 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 96 96] /Resources << /XObject << /Im0 {image} 0 R >> >> /Contents {contents} 0 R >> endobj\n\
                 {contents} 0 obj << /Length {} >> stream\n{draw}\nendstream endobj\n\
                 {image} 0 obj << /Type /XObject /Subtype /Image /Width 96 /Height 96 /ColorSpace /DeviceGray /BitsPerComponent 1 /Length 1152 >> stream\n",
                draw.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(&bits);
        pdf.extend_from_slice(b"\nendstream endobj\n");
    }
    pdf.extend_from_slice(b"trailer << /Root 1 0 R >>\n%%EOF");
    pdf
}

fn halftone_harness(scans: &[bool]) -> (Harness<'static, PdfCraftApp>, Arc<ManualJobs>) {
    open_harness(halftone(scans))
}

fn open_harness(bytes: Vec<u8>) -> (Harness<'static, PdfCraftApp>, Arc<ManualJobs>) {
    let (app, jobs) = crate::control::app_with_manual_jobs();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |cc| {
        crate::support::install_runtime(&cc.egui_ctx);
        let mut app = app;
        app.set_option(&egui::Context::default(), "language", "en").unwrap();
        app.open_bytes(&egui::Context::default(), "halftone.pdf", None, bytes).unwrap();
        app
    });
    h.run_steps(4);
    (h, jobs)
}

/// Enhance scanned file with the dialog's choices as they are, run to its end.
fn enhance(h: &mut Harness<'static, PdfCraftApp>, jobs: &ManualJobs) {
    assert!(h.execute("ocr.enhance"));
    h.run_steps(2);
    let ctx = h.ctx.clone();
    assert!(h.state_mut().dialogs.confirm::<HostedParamDialog>(&ctx));
    h.run_steps(2);
    run_jobs(h, jobs);
}

/// The result window's values, while it is open.
fn result(h: &Harness<'static, PdfCraftApp>) -> Option<serde_json::Value> {
    h.state().dialogs.open_dialogs().into_iter().find(|d| d.id == dialog_id("scan-result"))?.fields
}

/// Each restored page's text weight and text contrast.
fn tunings(h: &Harness<'static, PdfCraftApp>) -> Vec<(u64, u64)> {
    let fields = result(h).expect("the result window is open");
    let pages = fields["pages"].as_array().unwrap();
    pages.iter().map(|p| (p["text_weight"].as_u64().unwrap(), p["text_contrast"].as_u64().unwrap())).collect()
}

/// The result window's Next page button (the toolbar has one of its own, which comes first).
fn next_button<'a>(h: &'a Harness<'static, PdfCraftApp>) -> egui_kittest::Node<'a> {
    h.query_all_by_label("Next page").last().expect("a Next page button")
}

/// The result window's Close button (its title row has a close action of the same name, which comes first).
fn close_button<'a>(h: &'a Harness<'static, PdfCraftApp>) -> egui_kittest::Node<'a> {
    h.query_all_by_label("Close").last().expect("a Close button")
}

fn current_page(h: &Harness<'static, PdfCraftApp>) -> usize {
    h.state().views[h.state().active.unwrap()].current()
}

/// Moves a slider of the result window one step from the keyboard; the run it asks for waits in the job queue.
fn nudge(h: &mut Harness<'static, PdfCraftApp>, slider: &str, key: egui::Key) {
    h.get_by_role_and_label(Role::Slider, slider).focus();
    h.run_steps(2);
    h.key_press(key);
    h.run_steps(3);
}

fn undo_depth(h: &Harness<'static, PdfCraftApp>) -> usize {
    let app = h.state();
    app.session.get(app.active_ids().unwrap().1).unwrap().undo_depth()
}

/// The first page's pixels, as the document now stands (saved bytes carry the time).
fn pixels(h: &Harness<'static, PdfCraftApp>) -> Vec<u8> {
    let app = h.state();
    let bytes = app.session.get(app.active_ids().unwrap().1).unwrap().bytes.clone();
    let page = PageRenderer::new(bytes, Default::default()).render(RenderRequest { scale: 1.0, ..Default::default() });
    assert_eq!(page.error, None);
    assert!(!page.rgba.is_empty());
    page.rgba.to_vec()
}

#[test]
fn a_restored_halftone_scan_opens_a_movable_result_window_whose_sliders_redo_the_enhancement() {
    let (mut h, jobs) = halftone_harness(&[true]);
    enhance(&mut h, &jobs);
    assert!(result(&h).is_some(), "the result window opens");
    h.get_by_label_contains("1-bit halftone image format was detected on page 1 and was enhanced");
    assert!(next_button(&h).accesskit_node().is_disabled(), "the only restored page has no next");
    let title = |h: &Harness<'static, PdfCraftApp>| h.query_all_by_label("Scan enhanced").last().expect("a title").rect().center();
    let from = title(&h);
    h.drag_at(from);
    h.run_steps(1);
    h.hover_at(from + egui::vec2(120.0, 60.0));
    h.run_steps(1);
    h.drop_at(from + egui::vec2(120.0, 60.0));
    h.run_steps(2);
    assert_ne!(title(&h), from, "its title moves it");
    // A slider enhances again from the original: one step in the history, other pixels.
    let before = pixels(&h);
    nudge(&mut h, "Text weight", egui::Key::ArrowRight);
    run_jobs(&mut h, &jobs);
    assert_eq!(undo_depth(&h), 1, "the first enhancement was undone, not stacked");
    assert_ne!(pixels(&h), before, "the new setting changed the picture");
    close_button(&h).click();
    h.run_steps(2);
    assert!(result(&h).is_none());
    h.execute("edit.undo");
    h.run_steps(2);
    let plain = halftone_harness(&[true]).0;
    assert_eq!(pixels(&h), pixels(&plain), "one undo brings the scan back as it was");
}

#[test]
fn the_result_window_walks_the_restored_pages_and_only_ever_enhances_those() {
    // Page 2 is empty.
    let (mut h, jobs) = halftone_harness(&[true, false, true, true]);
    h.state_mut().views[0].go_to_page(1);
    enhance(&mut h, &jobs);
    assert_eq!(current_page(&h), 0, "the first restored page is shown");
    h.get_by_label_contains("on pages 1, 3, 4");
    assert_eq!(tunings(&h).len(), 3, "page 2 is none of the sliders'");
    for page in [2, 3] {
        next_button(&h).click();
        h.run_steps(3);
        assert_eq!(current_page(&h), page, "Next page skips the page that is no scan");
    }
    assert!(next_button(&h).accesskit_node().is_disabled(), "no next after the last restored page");
    // On the page that is no scan the sliders rest, unless they act on all.
    h.state_mut().views[0].go_to_page(1);
    h.run_steps(3);
    assert!(h.get_by_role_and_label(Role::Slider, "Text weight").accesskit_node().is_disabled());
    h.get_by_label("Apply to all").click();
    h.run_steps(2);
    assert!(!h.get_by_role_and_label(Role::Slider, "Text weight").accesskit_node().is_disabled());
}

/// Moving a slider from the keyboard tunes the page showing and enhances it again as one step, and
/// with Apply to all it tunes every restored page; only the pages it moved are restored again.
#[test]
fn moving_a_slider_tunes_the_page_showing_or_all_of_them_and_enhances_again() {
    let (mut h, jobs) = halftone_harness(&[true, false, true, true]);
    enhance(&mut h, &jobs);
    let first = tunings(&h);
    let (weight, contrast) = first[0];
    assert!(first.iter().all(|t| *t == first[0]), "every page starts alike");
    let before = pixels(&h);
    nudge(&mut h, "Text weight", egui::Key::ArrowRight);
    let run = h.state().scan_clean_run.as_ref().expect("the slider starts a run");
    assert_eq!(run.ask.again.as_deref(), Some(&[0][..]), "only the page it moved is restored again");
    // A slider moved while that run goes on waits for it.
    nudge(&mut h, "Text weight", egui::Key::ArrowRight);
    run_jobs(&mut h, &jobs);
    assert!(h.state().scan_clean_run.is_some(), "and then runs on its own");
    run_jobs(&mut h, &jobs);
    assert!(h.state().scan_clean_run.is_none());
    assert_eq!(tunings(&h), [(weight + 2, contrast), (weight, contrast), (weight, contrast)], "the page showing alone");
    assert_eq!(undo_depth(&h), 1, "enhanced again as the same one step");
    assert_ne!(pixels(&h), before, "with another picture");
    h.get_by_label("Apply to all").click();
    h.run_steps(2);
    nudge(&mut h, "Text contrast", egui::Key::ArrowLeft);
    assert_eq!(h.state().scan_clean_run.as_ref().unwrap().ask.again.as_deref(), Some(&[0, 2, 3][..]));
    run_jobs(&mut h, &jobs);
    assert_eq!(tunings(&h), [(weight + 2, contrast - 1); 3], "every restored page takes the sliders as they stand");
    assert_eq!(undo_depth(&h), 1);
    assert_eq!(result(&h).unwrap()["again"], serde_json::json!([]), "and nothing is left to do");
}

/// The window is there for the page beside it to be judged: the page still zooms while it is open,
/// and Escape closes it.
#[test]
fn the_page_zooms_beside_the_result_window_and_escape_closes_it() {
    let (mut h, jobs) = halftone_harness(&[true]);
    enhance(&mut h, &jobs);
    let zoom = h.state().views[0].zoom();
    // Over the page, clear of the window.
    h.hover_at(egui::pos2(1000.0, 600.0));
    h.run_steps(1);
    h.event(egui::Event::Zoom(1.5));
    h.run_steps(4);
    assert!(h.state().views[0].zoom() > zoom, "the page zooms");
    assert!(result(&h).is_some(), "and the window is still there");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(result(&h).is_none());
}

/// In full screen a click on the page turns it, but not beside the result window.
#[test]
fn full_screen_clicks_beside_the_result_window_do_not_turn_the_page() {
    let (mut h, jobs) = halftone_harness(&[true, true]);
    enhance(&mut h, &jobs);
    close_button(&h).click();
    h.run_steps(2);
    assert!(h.execute("edit.undo"));
    assert!(h.execute("view.full_screen"));
    h.run_steps(3);
    let ctx = h.ctx.clone();
    h.state_mut().start_scan_clean(&ctx);
    run_jobs(&mut h, &jobs);
    assert!(h.state().screen_mode.body_only(), "full screen");
    assert!(result(&h).is_some());
    // Over the page, clear of the window.
    let at = egui::pos2(1000.0, 600.0);
    let click = |h: &mut Harness<'static, PdfCraftApp>, button| {
        h.hover_at(at);
        h.run_steps(1);
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos: at, button, pressed, modifiers: Default::default() });
            h.run_steps(1);
        }
        // Past the double-click wait.
        h.run_steps(60);
    };
    click(&mut h, egui::PointerButton::Primary);
    assert_eq!(current_page(&h), 0, "a click beside the window turned the page forward");
    h.state_mut().views[0].go_to_page(1);
    h.run_steps(3);
    click(&mut h, egui::PointerButton::Secondary);
    assert_eq!(current_page(&h), 1, "a right-click beside the window turned the page back");
    assert!(result(&h).is_some(), "and the window is still there");
    // With the window closed the same click turns it.
    close_button(&h).click();
    h.run_steps(2);
    click(&mut h, egui::PointerButton::Secondary);
    assert_eq!(current_page(&h), 0, "the same click turns the page once the window is closed");
}

/// A slider's run takes the enhancement's place only once it is done: one that is cancelled, or whose
/// document moved on, leaves it.
#[test]
fn a_cancelled_retune_puts_the_enhancement_back() {
    let (mut h, jobs) = halftone_harness(&[true]);
    enhance(&mut h, &jobs);
    let enhanced = pixels(&h);
    nudge(&mut h, "Text weight", egui::Key::ArrowRight);
    assert_eq!(pixels(&h), enhanced, "the enhancement stays while the run goes on");
    h.state_mut().cancel_scan_clean();
    run_jobs(&mut h, &jobs);
    assert!(h.state().scan_clean_run.is_none());
    assert_eq!(undo_depth(&h), 1, "the enhancement is back");
    assert_eq!(pixels(&h), enhanced);
    // The sliders stay usable, and the page that kept its old picture is restored with the next change.
    assert_eq!(result(&h).unwrap()["again"], serde_json::json!([1]));
    nudge(&mut h, "Text weight", egui::Key::ArrowRight);
    run_jobs(&mut h, &jobs);
    assert_ne!(pixels(&h), enhanced);
    // A document that moves on meanwhile keeps both its enhancement and the change.
    nudge(&mut h, "Text weight", egui::Key::ArrowRight);
    let id = h.state().active_ids().unwrap().1;
    let shape = Shape::Note { at: [20.0, 20.0], icon: NoteIcon::Comment };
    let note = NewAnnotation { page: 0, style: Style::default_for(&shape), shape, contents: "Mine".into(), author: "Tester".into() };
    h.state_mut().session.apply(id, Edit::AddAnnotation(note)).unwrap();
    run_jobs(&mut h, &jobs);
    assert_eq!(undo_depth(&h), 2, "the enhancement and the note");
    assert!(h.state().session.get(id).unwrap().comment_props(0, 0).is_some());
    assert!(result(&h).is_none(), "the window belongs to the document as the run left it");
}

/// Pages that draw the same picture have one tuning between them.
#[test]
fn pages_sharing_a_scan_are_tuned_together() {
    let mut pdf = halftone(&[true, true]);
    // Page 2 draws page 1's picture.
    let (from, to) = (b"/Im0 7 0 R", b"/Im0 4 0 R");
    let at = pdf.windows(from.len()).position(|w| w == from).unwrap();
    pdf[at..at + to.len()].copy_from_slice(to);
    let (mut h, jobs) = open_harness(pdf);
    enhance(&mut h, &jobs);
    let before = tunings(&h);
    next_button(&h).click();
    h.run_steps(3);
    nudge(&mut h, "Text weight", egui::Key::ArrowRight);
    run_jobs(&mut h, &jobs);
    let after = tunings(&h);
    assert_eq!(after[0], after[1]);
    assert_ne!(after, before);
}

#[test]
fn a_window_closed_while_its_run_went_on_stays_closed() {
    let (mut h, jobs) = halftone_harness(&[true]);
    enhance(&mut h, &jobs);
    let before = pixels(&h);
    nudge(&mut h, "Text weight", egui::Key::ArrowRight);
    assert!(h.state().scan_clean_run.is_some());
    close_button(&h).click();
    h.run_steps(2);
    run_jobs(&mut h, &jobs);
    assert!(h.state().scan_clean_run.is_none() && result(&h).is_none(), "it does not come back");
    assert_eq!(undo_depth(&h), 1);
    assert_ne!(pixels(&h), before, "and the run's work is kept");
}

/// Restore halftone scans is a choice in the dialog: unticked, a dotted scan is left as it is and no
/// result window opens; ticked (the default), it is restored.
#[test]
fn unticking_restore_halftone_scans_leaves_a_dotted_scan_as_it_is() {
    let (mut h, jobs) = halftone_harness(&[true]);
    // Besides the halftone switch only edge shadow removal, which a one-bit scan does not take,
    // so the switch alone decides whether the page changes (and OK stays on without it).
    h.state_mut().scan_clean_draft.settings = pdfcraft_engine::scan_clean::ScanCleanSettings {
        level: 0,
        edge_shadows: true,
        despeckle: false,
        restore_halftones: true,
        ..Default::default()
    };
    for restore in [false, true] {
        assert!(h.execute("ocr.enhance"));
        h.run_steps(2);
        // Ticked to begin with, then as the last run left it.
        h.get_by_label("Restore halftone scans").scroll_to_me();
        h.run_steps(2);
        h.get_by_label("Restore halftone scans").click();
        h.run_steps(2);
        let ctx = h.ctx.clone();
        assert!(h.state_mut().dialogs.confirm::<HostedParamDialog>(&ctx));
        h.run_steps(2);
        run_jobs(&mut h, &jobs);
        assert_eq!(h.state().scan_clean_draft.settings.restore_halftones, restore, "the choice is remembered");
        assert_eq!(undo_depth(&h) > 0, restore, "restored only when ticked");
        assert_eq!(result(&h).is_some(), restore, "and only then is there a result to tune");
    }
}
