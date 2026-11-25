//! Scan & OCR ▸ Enhance scanned file: the dialog (pages, background removal level, edge
//! shadows, despeckle, halftone restore) and the background run with its progress card. The scans are cleaned
//! on the host's job queue ([`pdfcraft_engine::scan_clean`]); the result is applied as one undoable
//! step when it is done, unless the document changed meanwhile. A run that restored halftone scans
//! opens a result window ([`super::scan_clean_result`]), whose sliders run it again.

use std::sync::Arc;

use craft::pages::PageRange;
use craft::pages::ui::{PageRangeChoices, PageRangeOptions};
use craft::ui::contract::Bind;
use craft::ui::dialog::{DialogResult, dialog_id};
use craft::ui::patterns::form_dialogs::{DialogFields, HostedParamDialog, Param, ParamDialog, ParamKind};
use craft::ui::value_drag::SliderScale;
use pdfcraft_engine::scan_clean::{ScanCleanJob, ScanCleanReport, ScanCleanSettings, ScanCleaned};
use pdfcraft_engine::{DocId, EditError};

use super::job_count::JobCount;
use super::scan_clean_result::{self, ScanResultDialog};
use crate::PdfCraftApp;
use crate::notices::ProgressNotice;
use crate::notices::{self, ProgressKey};

/// The key the dialog and its result share.
const DIALOG_KEY: &str = "enhance-scan";

/// The dialog's choices (kept between runs).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ScanCleanDraft {
    pub range: PageRange,
    pub settings: ScanCleanSettings,
}

/// A running clean-up: its document and the count the job and the progress card share.
pub struct ScanCleanRun {
    pub doc: DocId,
    pub ask: ScanCleanAsk,
    pub progress: Arc<JobCount>,
}

/// What a clean-up was asked to do.
#[derive(Default)]
pub struct ScanCleanAsk {
    /// The pages (0-based).
    pub pages: Vec<usize>,
    pub settings: ScanCleanSettings,
    /// The pages (0-based) to restore again, when the run repeats the one the result window shows.
    pub again: Option<Vec<usize>>,
}

/// The settings the dialog's fields hold; a field that is absent keeps `base`'s value.
fn settings_from(fields: &DialogFields, base: ScanCleanSettings) -> ScanCleanSettings {
    let max = f64::from(pdfcraft_engine::camera_clean::MAX_LEVEL);
    ScanCleanSettings {
        level: fields.number("level").map_or(base.level, |n| n.round().clamp(0.0, max) as _),
        edge_shadows: fields.bool("edge_shadows").unwrap_or(base.edge_shadows),
        despeckle: fields.bool("despeckle").unwrap_or(base.despeckle),
        restore_halftones: fields.bool("restore_halftones").unwrap_or(base.restore_halftones),
        halftone: base.halftone,
    }
}

/// The dialog over `fields`: OK is on only while a filter would change something.
fn build_dialog(fields: &mut DialogFields, base: ScanCleanSettings, count: usize, current: usize) -> ParamDialog<'_> {
    let max = f64::from(pdfcraft_engine::camera_clean::MAX_LEVEL);
    let params = vec![
        Param::read_only_text(
            "note",
            String::new(),
            tl!("Cleans up the scanned pictures in place: recognized text, comments and links on the pages stay as they are."),
        )
        .full_width(),
        Param::section(tl!("Pages")),
        PageRangeOptions::param("pages", tl!("Pages"), count, current, PageRangeChoices::CurrentFromTo),
        Param::section(tl!("Filters")),
        Param::new("level", tl!("Background removal"), ParamKind::range(0.0, max, f64::from(base.level), SliderScale::Linear))
            .decimals(0)
            .tooltip(tl!("Evens out the paper so a grey or yellowed background turns white, and darkens faint strokes. 0 is off.")),
        Param::new("edge_shadows", tl!("Edge shadow removal"), ParamKind::bool(base.edge_shadows))
            .tooltip(tl!("Lightens shadows along the edges of the scan, such as a book's gutter or a copier's dark border")),
        Param::new("despeckle", tl!("Despeckle"), ParamKind::bool(base.despeckle))
            .tooltip(tl!("Removes isolated dark specks such as dust and scanner noise.")),
        Param::new("restore_halftones", tl!("Restore halftone scans"), ParamKind::bool(base.restore_halftones)).tooltip(tl!(
            "Turns a black and white scan that is fine dots all over, such as a fax of a faint page, back into clean grey with readable text. Pages of crisp text are left as they are."
        )),
    ];
    let ready = settings_from(fields, base).changes_something();
    ParamDialog::new(DIALOG_KEY, tl!("Enhance Scanned File"), params).ok_label(tl!("Enhance").into()).ok_enabled(ready).values(Bind::bound(fields))
}

/// What a finished run says: what was cleaned, what was not a scan, and what was left and why.
fn summary(r: &ScanCleanReport) -> String {
    let lang = crate::i18n::current();
    let mut out = if r.pages.is_empty() {
        tl!("No scanned pages were cleaned up.").to_string()
    } else {
        crate::i18n::trn(lang, r.pages.len() as u64, "Cleaned up the scan on {n} page.", "Cleaned up the scans on {n} pages.")
    };
    let restored: std::collections::BTreeSet<_> = r.halftone.iter().flat_map(|h| &h.pages).collect();
    if !restored.is_empty() {
        out.push(' ');
        out.push_str(&crate::i18n::trn(lang, restored.len() as u64, "{n} halftone page was restored.", "{n} halftone pages were restored."));
    }
    if !r.not_scanned.is_empty() {
        out.push(' ');
        out.push_str(&crate::i18n::trn(lang, r.not_scanned.len() as u64, "{n} page is not a scan.", "{n} pages are not scans."));
    }
    if let Some(first) = r.skipped.first() {
        out.push(' ');
        let page = first.pages.first().map_or_else(String::new, |p| (p + 1).to_string());
        // The reason is the engine's own sentence, in English.
        out.push_str(&crate::i18n::fmt(
            &crate::i18n::trn(
                lang,
                r.skipped.len() as u64,
                "{n} image was left as it was (page {p}: {why}).",
                "{n} images were left as they were (page {p}: {why}, …).",
            ),
            &[("p", &page), ("why", &first.reason)],
        ));
    }
    out
}

impl PdfCraftApp {
    /// Opens the Enhance Scanned File dialog over the remembered choices.
    pub fn open_enhance_scan(&mut self, ctx: &egui::Context) {
        let Some((vi, id)) = self.active_ids() else { return };
        let count = self.session.get(id).map_or(1, |d| d.info.pages.len().max(1));
        let current = self.views.get(vi).map_or(0, |v| v.current());
        let draft = self.scan_clean_draft.clone();
        let fields = DialogFields::from_pairs([
            ("pages", PageRangeOptions::field_value(&draft.range)),
            ("level", f64::from(draft.settings.level).into()),
            ("edge_shadows", draft.settings.edge_shadows.into()),
            ("despeckle", draft.settings.despeckle.into()),
            ("restore_halftones", draft.settings.restore_halftones.into()),
        ]);
        let base = draft.settings;
        let dialog = HostedParamDialog::new(ctx, DIALOG_KEY, fields, move |fields| build_dialog(fields, base, count, current));
        self.dialogs.open(dialog);
    }

    /// Once per frame: takes the dialog's answer, remembers it and starts the run.
    pub(crate) fn enhance_scan_pass(&mut self, ctx: &egui::Context) {
        let Some(DialogResult::Ok(answer)) = self.dialogs.take_by_id::<HostedParamDialog>(dialog_id(DIALOG_KEY)) else { return };
        let d = &mut self.scan_clean_draft;
        if let Some(range) = PageRangeOptions::value(&answer.fields, "pages") {
            d.range = range;
        }
        d.settings = settings_from(&answer.fields, d.settings);
        self.start_scan_clean(ctx);
    }

    /// Clean up the scans on the pages chosen in the dialog, in the background.
    pub fn start_scan_clean(&mut self, ctx: &egui::Context) {
        let Some((vi, id)) = self.active_ids() else { return };
        if !self.may_start_scan_clean(ctx) {
            return;
        }
        let count = self.session.get(id).map_or(0, |d| d.info.pages.len());
        let current = self.views.get(vi).map_or(0, |v| v.current());
        let d = &self.scan_clean_draft;
        let pages = d.range.pages(count, current, &[]);
        // The engine reads an empty list as every page, so a range that picks none must not reach it.
        if pages.is_empty() {
            self.notify_tr("There are no pages in that range");
            return;
        }
        let job = match self.session.scan_clean_job(id, &pages, d.settings) {
            Ok(job) => job,
            Err(e) => return self.notify(e.to_string()),
        };
        let ask = ScanCleanAsk { pages, settings: d.settings, again: None };
        self.submit_scan_clean(id, job, ask);
    }

    /// Enhance again the pages of the run the result window shows, with the sliders' tunings. The enhancement stays in the
    /// document until the new one takes its place, as the same one step, so a run that is cancelled or fails changes
    /// nothing. Only the pages a slider moved are restored again; every other picture of the run is kept as it is, so a
    /// nudge costs one page's restore, not the document's.
    pub(super) fn retune_scan_clean(&mut self, ctx: &egui::Context) {
        let Some(o) = self.dialogs.get::<ScanResultDialog>() else { return };
        let (doc, generation) = (o.doc, o.generation);
        // The typing in a form field goes in first; when that changes the document, the window closes saying so.
        if self.active_ids().map(|(_, id)| id) != Some(doc)
            || !self.may_start_scan_clean(ctx)
            || self.session.get(doc).map(|d| d.edit_generation()) != Some(generation)
        {
            return;
        }
        let Some(o) = self.dialogs.get_mut::<ScanResultDialog>() else { return };
        let (pages, settings, tunings) = (o.requested.clone(), o.settings, o.tunings.clone());
        let again = std::mem::take(&mut o.again);
        let job = self.session.scan_clean_kept(doc, o.halftone.clone()).and_then(|kept| {
            let job = self.session.scan_clean_again_job(doc, &pages, settings)?;
            Ok(job.with_page_tunings(tunings).keeping(kept, &again))
        });
        match job {
            Ok(job) => self.submit_scan_clean(doc, job, ScanCleanAsk { pages, settings, again: Some(again) }),
            Err(e) => {
                self.notify(e.to_string());
                scan_clean_result::not_done(self, doc, again);
            }
        }
    }

    /// Whether a run may start now: no other is going, and what's typed in a form field is in the document.
    fn may_start_scan_clean(&mut self, ctx: &egui::Context) -> bool {
        if self.scan_clean_run.is_some() {
            self.notify_tr("Scanned pages are already being enhanced");
            return false;
        }
        // Both jobs share the progress card and capture the same document state.
        if self.optimize_run.is_some() {
            self.notify_tr("Optimization is already running");
            return false;
        }
        // What's typed in a form field is part of the document (#166).
        self.commit_form_typing(ctx)
    }

    /// The page rasters, tiles and thumbnails show the old pixels at the same scale.
    fn invalidate_scan_pages(&mut self, doc: DocId) {
        for v in self.views.iter_mut().filter(|v| v.id == doc) {
            v.invalidate_content();
        }
    }

    /// Runs `job` on the host's queue.
    fn submit_scan_clean(&mut self, id: DocId, job: ScanCleanJob, ask: ScanCleanAsk) {
        let progress = Arc::new(JobCount::default());
        progress.set(0, job.total());
        let (count, wake) = (progress.clone(), self.wake.clone());
        self.scan_cleans.submit(id, Default::default(), move |_| {
            pdfcraft_engine::guard(|| {
                job.run(|done, total| {
                    count.set(done, total);
                    wake.wake();
                    !count.cancelled()
                })
            })
            .unwrap_or_else(|message| Err(EditError::Invalid(message)))
        });
        self.scan_clean_run = Some(ScanCleanRun { doc: id, ask, progress });
        self.post_scan_clean_progress();
    }

    /// Stop a running clean-up (nothing changes).
    pub fn cancel_scan_clean(&mut self) {
        let Some(run) = &self.scan_clean_run else { return };
        run.progress.cancel();
        self.post_scan_clean_progress();
    }

    /// Shows the run's progress card.
    fn post_scan_clean_progress(&mut self) {
        let Some(run) = &self.scan_clean_run else { return };
        let (done, total) = run.progress.read();
        let cancelling = run.progress.cancelled();
        let label = if cancelling {
            tl!("Cancelling…").to_string()
        } else {
            crate::i18n::fmt(
                tl!("Enhancing scans… image {d} of {t}"),
                &[("d", &(done + 1).min(total.max(1)).to_string()), ("t", &total.max(1).to_string())],
            )
        };
        let fraction = if total == 0 { 1.0 } else { done.min(total) as f32 / total as f32 };
        notices::set_progress(self, ProgressKey::ScanClean, ProgressNotice { label, fraction, cancellable: !cancelling });
    }

    /// Show progress; apply the result once the job is done.
    pub(crate) fn poll_scan_clean(&mut self) {
        let Some(run) = self.scan_clean_run.as_ref() else { return };
        let progress = run.progress.clone();
        let Some(finished) = self.scan_cleans.try_recv() else {
            if progress.take_change().is_some() {
                self.post_scan_clean_progress();
            }
            return;
        };
        notices::end_progress(self, ProgressKey::ScanClean);
        if let Some(run) = self.scan_clean_run.take() {
            self.land_scan_clean(run, finished.result);
        }
    }

    /// Applies a finished clean-up as one undoable step, says what it did, and shows the result window when it
    /// restored halftone scans.
    pub(crate) fn land_scan_clean(&mut self, run: ScanCleanRun, result: Result<ScanCleaned, EditError>) {
        let doc = run.doc;
        let again = run.ask.again.is_some();
        let applied = result.and_then(|cleaned| match again {
            true => self.session.replace_scan_clean(doc, cleaned),
            false => self.session.apply_scan_clean(doc, cleaned),
        });
        match applied {
            Ok(report) => {
                self.invalidate_scan_pages(doc);
                let msg = summary(&report);
                self.notify(msg);
                scan_clean_result::landed(self, run, report.halftone);
            }
            Err(e) => {
                match e {
                    EditError::Cancelled => self.notify_tr("Enhancing the scanned pages was cancelled"),
                    e => self.notify(e.to_string()),
                }
                if let Some(again) = run.ask.again {
                    scan_clean_result::not_done(self, doc, again);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfcraft_engine::scan_clean::SkippedImage;

    #[test]
    fn the_summary_says_what_was_done_and_what_was_left() {
        let r = ScanCleanReport {
            pages: vec![0, 2],
            images: 1,
            not_scanned: vec![1],
            skipped: vec![SkippedImage { pages: vec![3], name: "Im0".into(), reason: "it is CMYK".into() }],
            ..Default::default()
        };
        let said = summary(&r);
        assert!(said.contains('2') && said.contains('1') && said.contains('4'), "cleaned, not-a-scan and the skipped page: {said}");
        let restored = summary(&ScanCleanReport {
            pages: vec![0],
            halftone: vec![pdfcraft_engine::scan_clean::HalftoneImage {
                object: pdfcraft_cos::ObjRef::new(4, 0),
                format: String::new(),
                pages: vec![0],
            }],
            ..Default::default()
        });
        assert!(restored.contains("halftone"), "{restored}");
        let nothing = summary(&ScanCleanReport::default());
        assert_ne!(said, nothing);
        assert!(!nothing.chars().any(|c| c.is_ascii_digit()), "{nothing}");
    }
}
