//! Scan & OCR ▸ Enhance scanned file: the result window, shown after a run that restored halftone scans. It names the
//! restored pages and has sliders that tune them, the page showing or all of them; letting a slider go enhances again
//! (`PdfCraftApp::retune_scan_clean`).
//!
//! The window is not modal, so the page stays in view to be judged and can be scrolled and zoomed, but it belongs to
//! one document as the run left it: it closes when another document is shown or this one changes in any other way.

use std::collections::{BTreeMap, BTreeSet};

use craft::ui::choice::{Radio, RadioGroup};
use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _};
use craft::ui::dialog::{DialogId, DialogResult, DialogStack, StackDialog, dialog_id};
use craft::ui::display::Text;
use craft::ui::form::SliderField;
use craft::ui::patterns::form_dialogs::{FormButtons, FormDialog};
use craft::ui::runtime::focus::Initial;
use craft::ui::sections::{DialogRole, FooterButton};
use egui::{Align2, Context, Ui, pos2};
use pdfcraft_engine::DocId;
use pdfcraft_engine::scan_clean::{HalftoneImage, HalftoneTuning, ScanCleanSettings};
use serde_json::{Value as Json, json};

use super::scan_clean_ui::ScanCleanRun;
use crate::PdfCraftApp;

/// The name of the restoring tool, as the window says it.
const TOOL: &str = "Halftone Restore";

/// Next page's index among the footer buttons: the first after the form's own two.
const NEXT: usize = 2;

/// The result window, held as an open dialog of `app.dialogs`.
pub struct ScanResultDialog {
    pub(super) doc: DocId,
    /// The document's edit generation right after the run: the sliders undo and redo it only while nothing else has
    /// changed.
    pub(super) generation: u64,
    /// The pages (0-based) the run was asked for: the sliders repeat it on these, so the other scans it cleaned stay
    /// cleaned.
    pub(super) requested: Vec<usize>,
    /// The choices it ran with: the dialog's may have changed since.
    pub(super) settings: ScanCleanSettings,
    pub(super) halftone: Vec<HalftoneImage>,
    /// How each restored page (0-based, in order) is tuned: the only pages the sliders ever touch.
    pub(super) tunings: Vec<(usize, HalftoneTuning)>,
    /// The restored pages a slider has moved since the last run: the only ones the next run restores again.
    pub(super) again: Vec<usize>,
    /// A slider was let go: the pages are to be enhanced again.
    pub(super) pending: bool,
    /// The sliders move every restored page, not just the one showing.
    all: bool,
    /// The page showing, as [`pass`] last saw it.
    current: usize,
    /// The page Next page asked for.
    go_to: Option<usize>,
}

impl ScanResultDialog {
    /// Whether the sliders have a page to act on.
    fn acts(&self) -> bool {
        self.all || self.tunings.iter().any(|(p, _)| *p == self.current)
    }

    /// What the sliders show: the tuning of the page showing, or of the first restored page when it has none.
    fn shown(&self) -> HalftoneTuning {
        self.tunings.iter().find(|(p, _)| *p == self.current).or(self.tunings.first()).map_or_else(Default::default, |(_, t)| *t)
    }

    /// Gives the pages the sliders act on this tuning.
    fn tune(&mut self, tuning: HalftoneTuning) {
        let mut pages: BTreeSet<usize> = if self.all { self.tunings.iter().map(|(p, _)| *p).collect() } else { [self.current].into() };
        // A picture drawn on several pages has one tuning, so those pages go together.
        let mut linked = 0;
        while linked != pages.len() {
            linked = pages.len();
            for h in &self.halftone {
                if h.pages.iter().any(|p| pages.contains(p)) {
                    pages.extend(&h.pages);
                }
            }
        }
        for (page, own) in self.tunings.iter_mut().filter(|(p, t)| pages.contains(p) && *t != tuning) {
            *own = tuning;
            if !self.again.contains(page) {
                self.again.push(*page);
            }
        }
    }

    /// The restored page after the one showing.
    fn next(&self) -> Option<usize> {
        self.tunings.iter().map(|(p, _)| *p).find(|p| *p > self.current)
    }

    /// What was detected: one sentence a format, with all the pages that have it.
    fn detected(&self) -> Vec<String> {
        let mut formats = BTreeMap::<&str, BTreeSet<usize>>::new();
        for h in &self.halftone {
            formats.entry(&h.format).or_default().extend(&h.pages);
        }
        let sentence = |(format, pages): (&str, BTreeSet<usize>)| {
            let numbers: Vec<String> = pages.iter().map(|p| (p + 1).to_string()).collect();
            crate::i18n::fmt(
                &crate::i18n::trn(
                    crate::i18n::current(),
                    pages.len() as u64,
                    "A {format} image format was detected on page {pages} and was enhanced using the {tool} optimizing tool.",
                    "A {format} image format was detected on pages {pages} and was enhanced using the {tool} optimizing tool.",
                ),
                &[("format", format), ("pages", &numbers.join(", ")), ("tool", TOOL)],
            )
        };
        formats.into_iter().map(sentence).collect()
    }

    fn body(&mut self, ui: &mut Ui) {
        for sentence in self.detected() {
            Text::new(sentence).show(ui);
        }
        Text::new(tl!("Use the sliders below to customize the optimization further, otherwise, click 'Close'.")).show(ui);
        RadioGroup::new(tl!("Pages"), Bind::bound(&mut self.all)).show(ui, |ui, _| {
            Radio::new(false).label(tl!("Apply to current page")).show(ui);
            Radio::new(true).label(tl!("Apply to all")).show(ui);
        });
        if !self.acts() {
            Text::new(tl!("This page has no restored scan. Use 'Next page' to go to one.")).show(ui);
        }
        // Moving a slider only picks the value; the pages are enhanced again once it is let go (at once for a change
        // from the keyboard).
        let before = self.shown();
        let mut tuning = before;
        let enabled = self.acts();
        for (label, hint, value) in [
            (tl!("Text weight"), tl!("How heavy the strokes are. Raise it if the text looks thin or broken."), &mut tuning.weight),
            (tl!("Text contrast"), tl!("How dark the ink gets against the paper."), &mut tuning.contrast),
            (tl!("Background clean-up"), tl!("How much of the paper's speckle is cleared to white."), &mut tuning.cleanup),
        ] {
            let mut number = f64::from(*value);
            let out = SliderField::new(label, Bind::bound(&mut number), 0.0..=100.0).step(1.0).enabled(enabled).tooltip(hint).show(ui);
            *value = number.round().clamp(0.0, 100.0) as u8;
            self.pending |= out.edit.events.committed().is_some();
        }
        if tuning != before {
            self.tune(tuning);
        }
    }
}

impl StackDialog for ScanResultDialog {
    type Result = ();

    fn id(&self) -> DialogId {
        dialog_id("scan-result")
    }

    fn show(&mut self, ui: &mut Ui, _: &DialogStack) -> Option<DialogResult<()>> {
        let next = self.next();
        // The form's own value is not used: the body edits the window's state, which the app's pass reads.
        let mut unused = ();
        let shown = FormDialog::new(tl!("Scan enhanced"), Bind::bound(&mut unused))
            .buttons(FormButtons::Close)
            .cancel_label(tl!("Close"))
            .enter_confirms(false)
            .extra_buttons([FooterButton::new(tl!("Next page"), DialogRole::Other).enabled(next.is_some())])
            // Clear of the page's middle, so the page can be judged; not in a number field, so one Escape closes it.
            .shaped(|dialog| dialog.modeless().first_place(pos2(80.0, 110.0), Align2::LEFT_TOP).initial_focus(Initial::First))
            .show(ui, |ui, _, _| {
                self.body(ui);
                true
            });
        if shown.picked == Some(NEXT) {
            self.go_to = next;
        }
        shown.closed.map(|_| DialogResult::Cancel)
    }

    /// The restored pages with their tunings, whether the sliders act on all of them, and the pages the next run restores
    /// again.
    fn json_fields(&self) -> Option<Json> {
        let pages: Vec<Json> = self
            .tunings
            .iter()
            .map(|(p, t)| json!({"page": p + 1, "text_weight": t.weight, "text_contrast": t.contrast, "background_cleanup": t.cleanup}))
            .collect();
        let again: Vec<usize> = self.again.iter().map(|p| p + 1).collect();
        Some(json!({"all": self.all, "pages": pages, "again": again}))
    }
}

/// Brings the window of `doc` to the document as it is now, after a slider's run changed it.
pub(super) fn follow(app: &mut PdfCraftApp, doc: DocId) {
    let Some(generation) = app.session.get(doc).map(|d| d.edit_generation()) else { return };
    if let Some(o) = app.dialogs.get_mut::<ScanResultDialog>().filter(|o| o.doc == doc) {
        o.generation = generation;
    }
}

/// A slider's run that ended without a result: the pages `again` still have their old picture, and the next change of a
/// slider restores them too.
pub(super) fn not_done(app: &mut PdfCraftApp, doc: DocId, again: Vec<usize>) {
    if let Some(o) = app.dialogs.get_mut::<ScanResultDialog>().filter(|o| o.doc == doc) {
        o.again.extend(again);
        o.pending = false;
    }
}

/// After `run` was applied: a first run that restored `halftone` opens the window at the first restored page, and a
/// slider's run leaves the window ready for the next change. A window closed while its run went on stays closed.
pub(super) fn landed(app: &mut PdfCraftApp, run: ScanCleanRun, halftone: Vec<HalftoneImage>) {
    if run.ask.again.is_some() {
        return follow(app, run.doc);
    }
    let Some(generation) = app.session.get(run.doc).map(|d| d.edit_generation()) else { return };
    let pages: BTreeSet<usize> = halftone.iter().flat_map(|h| h.pages.iter().copied()).collect();
    let Some(first) = pages.first().copied() else { return };
    // It opens neither over another dialog nor for a document that is not the one showing.
    let Some((vi, _)) = app.active_ids().filter(|(_, id)| *id == run.doc) else { return };
    if app.modal_open() && !app.dialogs.is_open::<ScanResultDialog>() {
        return;
    }
    if let Some(v) = app.views.get_mut(vi) {
        v.go_to_page(first);
    }
    let tuning = run.ask.settings.halftone;
    let dialog = ScanResultDialog {
        doc: run.doc,
        generation,
        requested: run.ask.pages,
        settings: run.ask.settings,
        halftone,
        tunings: pages.into_iter().map(|p| (p, tuning)).collect(),
        again: Vec::new(),
        pending: false,
        all: false,
        current: first,
        go_to: None,
    };
    crate::dialogs::host::open(app, dialog);
}

/// Once per pass: closes the window when its document is no longer showing or changed, tells it the page showing, and
/// does what it asked for.
pub(crate) fn pass(app: &mut PdfCraftApp, ctx: &Context) {
    let Some(o) = app.dialogs.get::<ScanResultDialog>() else { return };
    let (doc, generation) = (o.doc, o.generation);
    let Some((vi, _)) = app.active_ids().filter(|(_, id)| *id == doc) else {
        app.dialogs.cancel::<ScanResultDialog>(ctx);
        return;
    };
    let busy = app.scan_clean_run.is_some();
    if app.session.get(doc).map(|d| d.edit_generation()) != Some(generation) {
        app.dialogs.cancel::<ScanResultDialog>(ctx);
        app.notify_tr("The document changed since it was enhanced: undo that step, then enhance again to use the sliders");
        return;
    }
    // A notice over the window keeps Escape from reaching it, and one is up after each run: the Escape nothing else took
    // closes it.
    if craft::ui::runtime::keys::with_window_intents(ctx, false, |window| window.take_exact(&craft::behavior::Intent::Cancel)) {
        app.dialogs.cancel::<ScanResultDialog>(ctx);
        return;
    }
    let Some(o) = app.dialogs.get_mut::<ScanResultDialog>() else { return };
    // A slider let go while a run goes on waits for it to land.
    let again = !busy && std::mem::take(&mut o.pending) && !o.again.is_empty();
    let go_to = o.go_to.take();
    if let Some(v) = app.views.get_mut(vi) {
        if let Some(page) = go_to {
            v.go_to_page(page);
        }
        if o.current != v.current() {
            o.current = v.current();
            ctx.request_repaint();
        }
    }
    if again {
        app.retune_scan_clean(ctx);
    }
}
