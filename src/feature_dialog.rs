//! The Create Feature / Edit Feature dialog.
//!
//! Covers everything a GenBank feature holds: its type (feature key), its location as
//! one or more spans, its strand, and any number of qualifiers. The name field is the
//! `/label` qualifier, which is what the editor and map draw.

use gb_io::seq::Feature as GbFeature;
use gpui::{
    anchored, deferred, div, point, prelude::*, px, Context, Entity, EventEmitter, FontWeight, MouseButton,
    SharedString, Subscription, Window,
};

use std::rc::Rc;

use crate::model::{self, Document, Feature};
use crate::pgx::{ChainLinking, Structure, STRUCTURE_QUALIFIER};
use crate::structure::{self, StructureFile};
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::workspace::Dismiss;

/// INSDC feature keys, the ones plasmid maps use most first. The rest follow the
/// INSDC Feature Table, plus the older regulatory keys many plasmid files still use.
pub const FEATURE_KEYS: &[&str] = &[
    "misc_feature", "CDS", "gene", "promoter", "terminator", "rep_origin", "primer_bind", "protein_bind", "RBS",
    "regulatory", "enhancer", "polyA_signal", "sig_peptide", "LTR", "oriT", "mRNA", "ncRNA", "misc_RNA", "tRNA",
    "rRNA", "5'UTR", "3'UTR", "exon", "intron", "mobile_element", "repeat_region", "misc_binding",
    "misc_difference", "misc_recomb", "misc_structure", "misc_signal", "stem_loop", "mat_peptide", "propeptide",
    "transit_peptide", "polyA_site", "precursor_RNA", "prim_transcript", "tmRNA", "operon", "variation",
    "modified_base", "STS", "source", "assembly_gap", "gap", "unsure", "old_sequence", "centromere", "telomere",
    "D-loop", "iDNA", "C_region", "D_segment", "J_segment", "N_region", "S_region", "V_region", "V_segment",
    "-10_signal", "-35_signal", "attenuator", "CAAT_signal", "TATA_signal", "GC_signal", "satellite",
    "conflict", "mutation", "misc_signal",
];

/// INSDC qualifiers, common ones first, then the rest of the INSDC list, plus a few
/// that plasmid editors write.
pub const QUALIFIER_KEYS: &[&str] = &[
    "note", "gene", "product", "locus_tag", "function", "standard_name", "translation", "codon_start",
    "transl_table", "protein_id", "db_xref", "EC_number", "label", "allele", "bound_moiety", "direction",
    "experiment", "inference", "regulatory_class", "rpt_type", "rpt_unit_seq", "rpt_unit_range", "rpt_family",
    "mobile_element_type", "ncRNA_class", "operon", "pseudo", "pseudogene", "gene_synonym", "old_locus_tag",
    "number", "phenotype", "partial", "citation", "map", "mod_base", "anticodon", "artificial_location",
    "circular_RNA", "compare", "exception", "frequency", "gap_type", "estimated_length", "linkage_evidence",
    "replace", "ribosomal_slippage", "tag_peptide", "trans_splicing", "transl_except", "PCR_conditions",
    "PCR_primers", "organism", "mol_type", "plasmid", "strain", "sub_strain", "isolate", "isolation_source",
    "host", "lab_host", "clone", "clone_lib", "sub_clone", "cell_line", "cell_type", "chromosome", "cultivar",
    "culture_collection", "bio_material", "specimen_voucher", "collection_date", "collected_by",
    "identified_by", "geo_loc_name", "country", "lat_lon", "altitude", "dev_stage", "ecotype",
    "environmental_sample", "focus", "germline", "haplogroup", "haplotype", "macronuclear", "mating_type",
    "metagenome_source", "organelle", "pop_variant", "proviral", "rearranged", "segment", "serotype", "serovar",
    "sex", "sub_species", "submitter_seqid", "tissue_lib", "tissue_type", "transgenic", "type_material",
    "variety", "satellite", "ApEinfo_fwdcolor", "ApEinfo_revcolor",
];

const MAX_SUGGESTIONS: usize = 8;

pub enum DialogEvent {
    /// The finished feature; `editing` is the index it replaces, if any.
    Confirm {
        feature: Feature,
        editing: Option<usize>,
        /// Structures attached from the dialog, to add to the document first.
        structures: Vec<Structure>,
    },
    Cancel,
}

struct Span {
    start: Entity<TextInput>,
    end: Entity<TextInput>,
}

struct QualifierRow {
    key: Entity<TextInput>,
    value: Entity<TextInput>,
}

pub struct FeatureDialog {
    /// The index of the feature being edited; None when creating one.
    editing: Option<usize>,
    seq_len: usize,
    circular: bool,
    name: Entity<TextInput>,
    kind: Entity<TextInput>,
    spans: Vec<Span>,
    strand: i8,
    qualifiers: Vec<QualifierRow>,
    error: Option<SharedString>,
    /// The sequence, for aligning structures to the feature.
    seq: Rc<Vec<u8>>,
    /// Structures already in the document, then those attached here.
    taken: Vec<Structure>,
    pending: Vec<Structure>,
    /// The structure chains this feature encodes: (structure id, chain).
    refs: Vec<(String, String)>,
    /// A structure file is being read and aligned.
    attaching: bool,
    /// Something to know about the last structure attached.
    notice: Option<SharedString>,
    /// The highlighted entry of the open suggestion list.
    highlighted: usize,
    subscriptions: Vec<Subscription>,
}

impl EventEmitter<DialogEvent> for FeatureDialog {}

impl FeatureDialog {
    /// A dialog for a new feature over `segments`.
    pub fn create(segments: &[(usize, usize)], doc: &Document, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self::empty(None, doc, "misc_feature", 1, window, cx);
        for (s, e) in spans_from_segments(segments, doc.len(), doc.circular) {
            this.add_span(&s.to_string(), &e.to_string(), window, cx);
        }
        if this.spans.is_empty() {
            this.add_span("", "", window, cx);
        }
        this.name.update(cx, |i, cx| i.focus(window, cx));
        this
    }

    /// A dialog to edit `feature`, the `ix`th of the document.
    pub fn edit(ix: usize, feature: &Feature, doc: &Document, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self::empty(Some(ix), doc, &feature.kind, feature.strand, window, cx);
        for (s, e) in spans_from_segments(&feature.segments, doc.len(), doc.circular) {
            this.add_span(&s.to_string(), &e.to_string(), window, cx);
        }
        // The first /label is the name; every other qualifier gets a row.
        let mut qualifiers = match &feature.source {
            Some(src) => src.qualifiers.clone(),
            None => vec![("label".into(), Some(feature.label.clone()))],
        };
        if let Some(i) = qualifiers.iter().position(|(k, _)| k == "label") {
            let (_, value) = qualifiers.remove(i);
            this.name.update(cx, |input, cx| input.set_text(value.unwrap_or_default(), cx));
        }
        for (k, v) in qualifiers {
            // Structure links get their own section.
            if k == STRUCTURE_QUALIFIER {
                if let Some((s, c)) = v.as_deref().and_then(|v| v.trim().split_once(':')) {
                    this.refs.push((s.to_string(), c.to_string()));
                    continue;
                }
            }
            this.add_qualifier(&k, v.as_deref().unwrap_or(""), window, cx);
        }
        this.name.update(cx, |i, cx| i.focus(window, cx));
        this
    }

    fn empty(editing: Option<usize>, doc: &Document, kind: &str, strand: i8, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| TextInput::new("", "Untitled", cx));
        let kind = cx.new(|cx| TextInput::new(kind, "misc_feature", cx));
        let subscriptions = vec![cx.subscribe_in(&name, window, Self::on_input), cx.subscribe_in(&kind, window, Self::on_input)];
        Self {
            editing,
            seq_len: doc.len(),
            circular: doc.circular,
            seq: doc.seq.clone(),
            taken: doc.structures.clone(),
            pending: Vec::new(),
            refs: Vec::new(),
            attaching: false,
            notice: None,
            name,
            kind,
            spans: Vec::new(),
            strand,
            qualifiers: Vec::new(),
            error: None,
            highlighted: 0,
            subscriptions,
        }
    }

    fn input(&mut self, text: &str, placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>) -> Entity<TextInput> {
        let input = cx.new(|cx| TextInput::new(text, placeholder, cx));
        self.subscriptions.push(cx.subscribe_in(&input, window, Self::on_input));
        input
    }

    fn add_span(&mut self, start: &str, end: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<TextInput> {
        let span = Span { start: self.input(start, "Start", window, cx), end: self.input(end, "End", window, cx) };
        let first = span.start.clone();
        self.spans.push(span);
        first
    }

    fn add_qualifier(&mut self, key: &str, value: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<TextInput> {
        let row = QualifierRow { key: self.input(key, "qualifier", window, cx), value: self.input(value, "value", window, cx) };
        let first = row.key.clone();
        self.qualifiers.push(row);
        first
    }

    /// Every field in Tab order.
    fn fields(&self) -> Vec<Entity<TextInput>> {
        let mut out = vec![self.name.clone(), self.kind.clone()];
        out.extend(self.spans.iter().flat_map(|s| [s.start.clone(), s.end.clone()]));
        out.extend(self.qualifiers.iter().flat_map(|q| [q.key.clone(), q.value.clone()]));
        out
    }

    /// The suggestion field that has focus, its suggestions, and where a pick moves to.
    fn open_suggestions(&self, window: &Window, cx: &Context<Self>) -> Option<(Entity<TextInput>, Vec<&'static str>, Option<Entity<TextInput>>)> {
        let fields = std::iter::once((&self.kind, FEATURE_KEYS, None))
            .chain(self.qualifiers.iter().map(|q| (&q.key, QUALIFIER_KEYS, Some(q.value.clone()))));
        for (input, list, next) in fields {
            let i = input.read(cx);
            if i.is_focused(window) {
                let found = suggestions(list, i.text());
                return (!found.is_empty()).then(|| (input.clone(), found, next));
            }
        }
        None
    }

    fn pick(&mut self, input: &Entity<TextInput>, value: &str, next: Option<Entity<TextInput>>, window: &mut Window, cx: &mut Context<Self>) {
        input.update(cx, |i, cx| i.set_text(value, cx));
        if let Some(next) = next {
            next.update(cx, |i, cx| i.focus(window, cx));
        }
        cx.notify();
    }

    fn on_input(&mut self, input: &Entity<TextInput>, ev: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match *ev {
            InputEvent::Changed => {
                self.highlighted = 0;
                self.error = None;
                cx.notify();
            }
            InputEvent::Up => {
                self.highlighted = self.highlighted.saturating_sub(1);
                cx.notify();
            }
            InputEvent::Down => {
                self.highlighted = (self.highlighted + 1).min(MAX_SUGGESTIONS - 1);
                cx.notify();
            }
            InputEvent::Submit => {
                // Enter picks the highlighted suggestion when a list is open; otherwise confirms.
                if let Some((field, found, next)) = self.open_suggestions(window, cx).filter(|(f, ..)| f == input) {
                    let value = found[self.highlighted.min(found.len() - 1)];
                    self.pick(&field, value, next, window, cx);
                } else {
                    self.confirm(cx);
                }
            }
            InputEvent::Tab { back } => {
                let fields = self.fields();
                if let Some(i) = fields.iter().position(|f| f == input) {
                    let n = fields.len();
                    let to = if back { (i + n - 1) % n } else { (i + 1) % n };
                    fields[to].update(cx, |f, cx| f.focus(window, cx));
                    self.highlighted = 0;
                    cx.notify();
                }
            }
        }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        match self.build(cx) {
            Ok(feature) => {
                // Only structures something still links to.
                let structures = self.pending.iter().filter(|s| self.refs.iter().any(|(id, _)| *id == s.id)).cloned().collect();
                cx.emit(DialogEvent::Confirm { feature, editing: self.editing, structures })
            }
            Err(e) => {
                self.error = Some(e.into());
                cx.notify();
            }
        }
    }

    /// The feature the form describes, or what's wrong with it.
    fn build(&self, cx: &Context<Self>) -> Result<Feature, String> {
        let text = |i: &Entity<TextInput>| i.read(cx).text().trim().to_string();
        let kind = text(&self.kind);
        if kind.is_empty() {
            return Err("Choose a feature type".into());
        }
        if kind.chars().any(char::is_whitespace) {
            return Err("Feature types can't contain spaces".into());
        }
        let mut spans = Vec::new();
        for (n, span) in self.spans.iter().enumerate() {
            let (s, e) = (text(&span.start), text(&span.end));
            if s.is_empty() && e.is_empty() && self.spans.len() > 1 {
                continue;
            }
            let start = parse_position(&s).ok_or_else(|| format!("Span {}: start isn't a position", n + 1))?;
            let end = parse_position(&e).ok_or_else(|| format!("Span {}: end isn't a position", n + 1))?;
            spans.push((start, end));
        }
        let segments = segments_from_spans(&spans, self.seq_len, self.circular)?;

        let mut qualifiers: Vec<(std::borrow::Cow<'static, str>, Option<String>)> = Vec::new();
        let name = text(&self.name);
        if !name.is_empty() {
            qualifiers.push(("label".into(), Some(name)));
        }
        for q in &self.qualifiers {
            let key = text(&q.key);
            // Values keep their spacing; only the key is trimmed.
            let value = q.value.read(cx).text().to_string();
            if key.is_empty() {
                if value.trim().is_empty() {
                    continue;
                }
                return Err(format!("The qualifier with value \"{}\" needs a name", value.trim()));
            }
            if key.chars().any(char::is_whitespace) {
                return Err(format!("Qualifier \"{key}\" can't contain spaces"));
            }
            if key == "transl_table" && value.trim().parse::<u8>().ok().and_then(crate::genetic_code::Code::get).is_none() {
                let ids: Vec<String> = crate::genetic_code::CODES.iter().map(|c| c.id.to_string()).collect();
                return Err(format!("/transl_table must be an NCBI genetic code: {}", ids.join(", ")));
            }
            // An empty value is a flag such as /pseudo.
            qualifiers.push((key.into(), (!value.trim().is_empty()).then_some(value)));
        }
        for (id, chain) in &self.refs {
            qualifiers.push((STRUCTURE_QUALIFIER.into(), Some(format!("{id}:{chain}"))));
        }

        let location = model::segments_location(&segments, self.strand).expect("at least one segment");
        let source = GbFeature { kind: kind.clone().into(), location, qualifiers };
        Ok(Feature { kind, label: model::feature_label(&source), segments, strand: self.strand, source: Some(source) })
    }

    /// Pick a structure file and link the chains that this feature encodes. The file
    /// joins the document when the dialog is confirmed.
    fn attach_structure(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let feature = match self.build(cx) {
            Ok(f) => f,
            Err(e) => {
                self.error = Some(format!("{e} before attaching a structure").into());
                cx.notify();
                return;
            }
        };
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Attach Structure".into()),
        });
        let seq = self.seq.to_vec();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            this.update(cx, |d, cx| {
                d.attaching = true;
                d.notice = None;
                d.error = None;
                cx.notify();
            })
            .ok();
            // Read, then align every protein chain to this feature's translation.
            let result = cx
                .background_executor()
                .spawn(async move {
                    let file = StructureFile::read(&path)?;
                    let mut chains: Vec<(String, f32, usize, bool)> = structure::protein_chains(&file.mol)
                        .into_iter()
                        .filter_map(|ci| {
                            let link = structure::link_chain_to_feature(&file.mol, ci, &seq, 0, &feature)?;
                            let (letters, _) = structure::chain_letters(&file.mol, ci);
                            let protein_len = feature.segments.iter().map(|(s, e)| e - s).sum::<usize>() / 3;
                            let convincing = link.convincing(letters.len(), protein_len);
                            Some((file.mol.chains[ci].id.clone(), link.identity, link.aligned, convincing))
                        })
                        .collect();
                    chains.sort_by(|a, b| b.3.cmp(&a.3).then((b.2 as f32 * b.1).total_cmp(&(a.2 as f32 * a.1))));
                    let protein = !structure::protein_chains(&file.mol).is_empty();
                    Ok::<_, String>((file, chains, protein))
                })
                .await;
            this.update(cx, |d, cx| {
                d.attaching = false;
                match result {
                    Err(e) => d.error = Some(e.into()),
                    Ok((file, _, false)) => d.error = Some(format!("{} has no protein chains to link", file.name).into()),
                    Ok((file, chains, true)) => {
                        let taken: Vec<Structure> = d.taken.iter().chain(&d.pending).cloned().collect();
                        let s = file.to_structure(&taken, ChainLinking::Features);
                        let good: Vec<&String> = chains.iter().filter(|c| c.3).map(|c| &c.0).collect();
                        let linked: Vec<String> = if good.is_empty() {
                            // Nothing matches well: link the closest and say so.
                            let best = chains.first();
                            d.notice = Some(match best {
                                Some((c, identity, _, _)) => {
                                    format!("No chain of {} matches this feature well; linked chain {c} ({:.0}% identical)", file.name, identity * 100.0)
                                }
                                None => format!("No chain of {} aligns to this feature", file.name),
                            }
                            .into());
                            best.map(|c| vec![c.0.clone()]).unwrap_or_default()
                        } else {
                            good.into_iter().cloned().collect()
                        };
                        if !linked.is_empty() {
                            d.refs.extend(linked.into_iter().map(|c| (s.id.clone(), c)));
                            d.pending.push(s);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The name to show for structure `id`.
    fn structure_name(&self, id: &str) -> String {
        self.taken
            .iter()
            .chain(&self.pending)
            .find(|s| s.id == id)
            .map_or_else(|| id.to_string(), |s| s.source.clone().unwrap_or_else(|| s.name.clone()))
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Cancel);
    }
}

impl Render for FeatureDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let open = self.open_suggestions(window, cx);
        let highlighted = self.highlighted;

        let label = |text: &'static str| {
            div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme::muted_text()).child(text)
        };
        let hint = |text: &'static str| div().text_xs().text_color(theme::muted_text()).child(text);
        let link = |id: SharedString, text: &'static str| {
            div()
                .id(id)
                .text_xs()
                .text_color(theme::accent())
                .cursor_pointer()
                .hover(|s| s.underline())
                .child(text)
        };
        let remove = |id: SharedString| {
            div()
                .id(id)
                .flex_none()
                .w(px(22.0))
                .h(px(22.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .text_color(theme::muted_text())
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()).text_color(theme::text()))
                .child("×")
        };
        let chips: Vec<_> = self
            .refs
            .iter()
            .enumerate()
            .map(|(i, (id, chain))| {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .pl_2()
                    .rounded_md()
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::editor_bg())
                    .child(format!("{} · chain {chain}", self.structure_name(id)))
                    .child(remove(format!("remove-structure-{i}").into()).on_click(cx.listener(move |this, _, _, cx| {
                        this.refs.remove(i);
                        cx.notify();
                    })))
            })
            .collect();
        let button = |id: &'static str, text: &'static str, primary: bool| {
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_md()
                .text_sm()
                .border_1()
                .cursor_pointer()
                .when(primary, |d| d.border_color(theme::accent()).bg(theme::accent()).text_color(theme::editor_bg()))
                .when(!primary, |d| d.border_color(theme::border()).hover(|s| s.bg(theme::hover())))
                .child(text)
        };

        // The suggestion list hangs under its field, over the rest of the dialog. It's a
        // child of the backdrop (which sits at the window origin) rather than of the
        // field, so the dialog's scrolling body doesn't clip it.
        let dropdown = open.as_ref().and_then(|(field, found, next)| {
            let at = field.read(cx).bounds()?;
            let items = found.iter().enumerate().map(|(i, &value)| {
                let field = field.clone();
                let next = next.clone();
                div()
                    .id(("suggestion", i))
                    .px_2()
                    .py(px(2.0))
                    .rounded_sm()
                    .cursor_pointer()
                    .when(i == highlighted.min(found.len() - 1), |d| d.bg(theme::active()))
                    .hover(|s| s.bg(theme::hover()))
                    .child(value)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.pick(&field, value, next.clone(), window, cx);
                        }),
                    )
            });
            // The field's padding and border sit around the painted text area.
            let left = at.left() - px(9.0);
            let below = at.bottom() + px(5.0);
            let height = px(found.len() as f32 * 22.0 + 12.0);
            let top = if below + height > viewport.height - px(8.0) { at.top() - px(5.0) - height } else { below };
            Some(
                div()
                    .id("feature-suggestions")
                    .occlude()
                    .absolute()
                    .left(left)
                    .top(top)
                    .w((at.size.width + px(18.0)).max(px(220.0)))
                    .p_1()
                    .flex()
                    .flex_col()
                    .rounded_md()
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::panel())
                    .shadow_md()
                    .text_sm()
                    .text_color(theme::text())
                    .children(items),
            )
        });

        let spans = self.spans.iter().enumerate().map(|(i, span)| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(div().w(px(120.0)).child(span.start.clone()))
                .child(div().text_color(theme::muted_text()).child("to"))
                .child(div().w(px(120.0)).child(span.end.clone()))
                .when(self.spans.len() > 1, |d| {
                    d.child(remove(format!("remove-span-{i}").into()).on_click(cx.listener(move |this, _, _, cx| {
                        this.spans.remove(i);
                        cx.notify();
                    })))
                })
        });
        let spans: Vec<_> = spans.collect();

        let mut qualifier_rows = Vec::new();
        for (i, q) in self.qualifiers.iter().enumerate() {
            qualifier_rows.push(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap_2()
                    .child(div().w(px(170.0)).flex_none().child(q.key.clone()))
                    .child(div().flex_1().min_w_0().child(q.value.clone()))
                    .child(remove(format!("remove-qualifier-{i}").into()).on_click(cx.listener(move |this, _, _, cx| {
                        this.qualifiers.remove(i);
                        cx.notify();
                    }))),
            );
        }

        let strand_choice = |id: &'static str, text: &'static str, value: i8| {
            let selected = self.strand == value;
            div()
                .id(id)
                .px_3()
                .py(px(3.0))
                .text_sm()
                .cursor_pointer()
                .when(selected, |d| d.bg(theme::active()).text_color(theme::accent()))
                .when(!selected, |d| d.hover(|s| s.bg(theme::hover())))
                .child(text)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.strand = value;
                    cx.notify();
                }))
        };

        let title = if self.editing.is_some() { "Edit Feature" } else { "Create Feature" };
        let body = div()
            .id("feature-dialog-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .child(div().flex().flex_col().gap_1().child(label("NAME")).child(self.name.clone()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(label("TYPE"))
                    .child(self.kind.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(label("LOCATION"))
                    .children(spans)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_3()
                            .items_center()
                            .child(link("add-span".into(), "+ Add span").on_click(cx.listener(|this, _, window, cx| {
                                let first = this.add_span("", "", window, cx);
                                first.update(cx, |i, cx| i.focus(window, cx));
                                cx.notify();
                            })))
                            .child(hint(if self.circular {
                                "Bases, 1-based and inclusive. A start after its end runs through the origin."
                            } else {
                                "Bases, 1-based and inclusive."
                            })),
                    ),
            )
            .child(
                div().flex().flex_col().gap_1().child(label("STRAND")).child(
                    div()
                        .flex()
                        .flex_row()
                        .w_auto()
                        .rounded_md()
                        .border_1()
                        .border_color(theme::border())
                        .overflow_hidden()
                        .child(strand_choice("strand-fwd", "Forward  →", 1))
                        .child(div().w(px(1.0)).bg(theme::border()))
                        .child(strand_choice("strand-rev", "←  Reverse", -1))
                        .child(div().w(px(1.0)).bg(theme::border()))
                        .child(strand_choice("strand-none", "None", 0)),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(label("QUALIFIERS"))
                    .children(qualifier_rows)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_3()
                            .items_center()
                            .child(link("add-qualifier".into(), "+ Add qualifier").on_click(cx.listener(|this, _, window, cx| {
                                let key = this.add_qualifier("", "", window, cx);
                                key.update(cx, |i, cx| i.focus(window, cx));
                                cx.notify();
                            })))
                            .child(hint("Leave the value empty for flags such as /pseudo.")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(label("STRUCTURES"))
                    .when(!self.refs.is_empty(), |d| d.child(div().flex().flex_row().flex_wrap().gap_2().children(chips)))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_3()
                            .items_center()
                            .when(!self.attaching, |d| {
                                d.child(link("attach-structure".into(), "+ Attach structure…").on_click(cx.listener(|this, _, window, cx| this.attach_structure(window, cx))))
                            })
                            .child(hint(if self.attaching {
                                "Reading and aligning…"
                            } else {
                                "PDB or mmCIF. Chains that this feature encodes are linked to it."
                            })),
                    )
                    .when_some(self.notice.clone(), |d, n| d.child(div().text_xs().text_color(theme::muted_text()).child(n))),
            );

        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(theme::border())
            .child(div().flex_1().text_sm().text_color(theme::error()).children(self.error.clone()))
            .child(button("feature-cancel", "Cancel", false).on_click(cx.listener(|this, _, _, cx| this.cancel(cx))))
            .child(button("feature-confirm", "Confirm", true).on_click(cx.listener(|this, _, _, cx| this.confirm(cx))));

        let panel = div()
            .id("feature-dialog")
            .occlude()
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.cancel(cx)))
            .flex()
            .flex_col()
            .w(px(560.0))
            .max_h(viewport.height - px(80.0))
            .rounded_lg()
            .border_1()
            .border_color(theme::border())
            .bg(theme::panel())
            .shadow_lg()
            .text_sm()
            .text_color(theme::text())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(hint(if self.circular { "circular sequence" } else { "linear sequence" })),
            )
            .child(body)
            .child(footer);

        // Cover the whole window so the rest of the app waits for an answer.
        deferred(
            anchored().position(point(px(0.0), px(0.0))).child(
                div()
                    .id("feature-dialog-backdrop")
                    .occlude()
                    .relative()
                    .w(viewport.width)
                    .h(viewport.height)
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::hsla(0.0, 0.0, 0.0, 0.35))
                    .child(panel)
                    .children(dropdown),
            ),
        )
        .with_priority(2)
    }
}

/// Up to `MAX_SUGGESTIONS` entries of `list` containing `text` (any case), prefix
/// matches first. None once `text` is exactly an entry.
pub fn suggestions(list: &'static [&'static str], text: &str) -> Vec<&'static str> {
    let t = text.trim().to_ascii_lowercase();
    if list.contains(&text.trim()) {
        return Vec::new();
    }
    let mut prefix: Vec<&str> = Vec::new();
    let mut inner: Vec<&str> = Vec::new();
    for &item in list {
        let l = item.to_ascii_lowercase();
        if l.starts_with(&t) {
            prefix.push(item);
        } else if l.contains(&t) {
            inner.push(item);
        }
    }
    prefix.dedup();
    prefix.into_iter().chain(inner).take(MAX_SUGGESTIONS).collect()
}

/// A position typed by a person: digits, maybe with thousands separators.
fn parse_position(s: &str) -> Option<usize> {
    let digits: String = s.chars().filter(|c| !matches!(c, ',' | '_' | ' ')).collect();
    digits.parse().ok()
}

/// Feature segments (0-based, half-open, sorted) as spans a person reads: 1-based and
/// inclusive. On a circular sequence the two segments of a feature that runs through
/// the origin become one span whose start is after its end, listed first.
pub fn spans_from_segments(segments: &[(usize, usize)], len: usize, circular: bool) -> Vec<(usize, usize)> {
    let mut segs = segments.to_vec();
    segs.sort_unstable();
    let mut spans = Vec::new();
    if circular && segs.len() >= 2 && segs[0].0 == 0 && segs[segs.len() - 1].1 == len {
        let head = segs.remove(0);
        let tail = segs.pop().expect("two or more");
        spans.push((tail.0 + 1, head.1));
    }
    spans.extend(segs.into_iter().map(|(s, e)| (s + 1, e)));
    spans
}

/// The inverse of [`spans_from_segments`], checking each span fits the sequence.
pub fn segments_from_spans(spans: &[(usize, usize)], len: usize, circular: bool) -> Result<Vec<(usize, usize)>, String> {
    if spans.is_empty() {
        return Err("Give the feature a location".into());
    }
    let mut segs = Vec::new();
    for (n, &(start, end)) in spans.iter().enumerate() {
        let which = if spans.len() > 1 { format!("Span {}: ", n + 1) } else { String::new() };
        for p in [start, end] {
            if p == 0 || p > len {
                return Err(format!("{which}{p} is outside the sequence (1–{len})"));
            }
        }
        if start <= end {
            segs.push((start - 1, end));
        } else if circular {
            segs.push((start - 1, len));
            segs.push((0, end));
        } else {
            return Err(format!("{which}start {start} is after end {end}"));
        }
    }
    segs.sort_unstable();
    Ok(segs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_round_trip_through_the_origin() {
        let segs = vec![(0, 3), (5, 6), (8, 10)];
        let spans = spans_from_segments(&segs, 10, true);
        assert_eq!(spans, vec![(9, 3), (6, 6)]);
        assert_eq!(segments_from_spans(&spans, 10, true).unwrap(), segs);
        // Linear: the same segments are just three spans.
        assert_eq!(spans_from_segments(&segs, 10, false), vec![(1, 3), (6, 6), (9, 10)]);
    }

    #[test]
    fn spans_are_checked() {
        assert!(segments_from_spans(&[(0, 3)], 10, false).is_err());
        assert!(segments_from_spans(&[(2, 11)], 10, false).is_err());
        assert!(segments_from_spans(&[(5, 2)], 10, false).is_err());
        assert_eq!(segments_from_spans(&[(5, 2)], 10, true).unwrap(), vec![(0, 2), (4, 10)]);
        assert!(segments_from_spans(&[], 10, true).is_err());
        assert_eq!(parse_position(" 1,234 "), Some(1234));
    }

    #[test]
    fn suggestions_prefer_prefixes_and_stop_on_exact_match() {
        assert_eq!(suggestions(FEATURE_KEYS, "CDS"), Vec::<&str>::new());
        let s = suggestions(FEATURE_KEYS, "prom");
        assert_eq!(s.first(), Some(&"promoter"));
        let s = suggestions(QUALIFIER_KEYS, "transl");
        assert!(s.starts_with(&["translation", "transl_table", "transl_except"]));
        assert!(suggestions(FEATURE_KEYS, "").len() == MAX_SUGGESTIONS);
    }
}
