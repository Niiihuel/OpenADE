//! The card for a question the agent asked with `ask_user`: one question at
//! a time, its options as rows, a field for a different answer, the images
//! attached to it, and what the user answered once it is sent. The request is
//! the same durable one a permission prompt uses; this module only draws it
//! and builds the answer.

use std::{collections::BTreeSet, sync::Arc};

use openide_core::{UserImage, UserImageMime, MAX_TOOL_MEDIA_BYTES, MAX_TOOL_MEDIA_IMAGES};
use openide_ui::{
    Attachment, Button, ButtonGroup, ButtonVariant, Card, Component, ControlSize, Element, Icon,
    RasterImage, RasterImageFormat, Skeleton, Text, TextArea, TextRole,
};
use serde_json::{json, Value};

use crate::{
    theme, tool_card_skeleton,
    transcript::TranscriptItem,
    view::{PermissionView, ViewState},
};

pub(crate) const TOOL: &str = openide_harness::ASK_USER_TOOL;
const MAX_QUESTIONS: usize = 4;
const MAX_OPTIONS: usize = 4;
pub(crate) const MAX_TEXT_BYTES: usize = 4000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Question {
    pub header: String,
    pub question: String,
    pub options: Vec<(String, String)>,
    pub multi_select: bool,
}

/// An image attached to an answer: read, verified and within what a model is
/// given when it was attached. The bytes are held once, as the card paints
/// them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct QuestionImage {
    pub label: String,
    pub raster: RasterImage,
}

impl QuestionImage {
    pub(crate) fn new(label: impl Into<String>, image: UserImage) -> Option<Self> {
        let format = match image.mime_type {
            UserImageMime::Png => RasterImageFormat::Png,
            UserImageMime::Jpeg => RasterImageFormat::Jpeg,
            UserImageMime::Webp => RasterImageFormat::Webp,
            UserImageMime::Gif => RasterImageFormat::Gif,
        };
        Some(Self {
            label: label.into(),
            raster: RasterImage::new(
                image.sha256,
                format,
                image.size_bytes,
                Arc::from(image.data_base64),
                "Imagen adjunta a la respuesta",
            )?,
        })
    }

    fn user_image(&self) -> UserImage {
        UserImage {
            mime_type: match self.raster.format() {
                RasterImageFormat::Png => UserImageMime::Png,
                RasterImageFormat::Jpeg => UserImageMime::Jpeg,
                RasterImageFormat::Webp => UserImageMime::Webp,
                RasterImageFormat::Gif => UserImageMime::Gif,
            },
            size_bytes: self.raster.size_bytes(),
            sha256: self.raster.sha256().to_owned(),
            data_base64: self.raster.data_base64().to_owned(),
        }
    }
}

/// What the user has chosen so far, per question. Kept by request id, so it
/// survives the card being rebuilt and the session being reselected.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct QuestionDraft {
    pub page: usize,
    pub picked: Vec<BTreeSet<usize>>,
    pub text: Vec<String>,
    /// The images attached to each question, in the order they were added.
    pub images: Vec<Vec<QuestionImage>>,
    /// Why the last image was not attached. The card says it: the status
    /// line is not part of the conversation.
    pub image_error: Option<String>,
}

/// The questions of a call, bounded as the tool's schema bounds them. Fields
/// that have not arrived yet are simply absent.
pub(crate) fn parse(arguments: &Value) -> Vec<Question> {
    let text = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    arguments
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_QUESTIONS)
        .map(|question| Question {
            header: text(question, "header"),
            question: text(question, "question"),
            options: question
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .take(MAX_OPTIONS)
                .map(|option| (text(option, "label"), text(option, "description")))
                .collect(),
            multi_select: question
                .get("multi_select")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
        .collect()
}

impl QuestionDraft {
    fn fit(&mut self, questions: usize) {
        self.picked.resize_with(questions, BTreeSet::new);
        self.text.resize_with(questions, String::new);
        self.images.resize_with(questions, Vec::new);
        self.page = self.page.min(questions.saturating_sub(1));
    }

    pub(crate) fn pick(&mut self, questions: &[Question], question: usize, option: usize) {
        self.fit(questions.len());
        let Some(asked) = questions.get(question) else {
            return;
        };
        if option >= asked.options.len() {
            return;
        }
        let picked = &mut self.picked[question];
        if asked.multi_select {
            if !picked.remove(&option) {
                picked.insert(option);
            }
        } else if picked.contains(&option) {
            picked.clear();
        } else {
            *picked = BTreeSet::from([option]);
        }
    }

    pub(crate) fn turn(&mut self, questions: usize, forward: bool) {
        self.fit(questions);
        self.page = if forward {
            (self.page + 1).min(questions.saturating_sub(1))
        } else {
            self.page.saturating_sub(1)
        };
    }

    pub(crate) fn write(&mut self, questions: usize, question: usize, value: &str) {
        self.fit(questions);
        if let Some(text) = self.text.get_mut(question) {
            let mut end = value.len().min(MAX_TEXT_BYTES);
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            *text = value[..end].to_owned();
        }
    }

    /// Attaches an image to a question. An answer's images travel as one
    /// tool result's, so they share that result's limits.
    pub(crate) fn attach(
        &mut self,
        questions: usize,
        question: usize,
        image: QuestionImage,
    ) -> Result<(), String> {
        self.fit(questions);
        let attached = self.images.iter().flatten();
        if attached.clone().count() >= MAX_TOOL_MEDIA_IMAGES {
            return Err(format!("Máximo de {MAX_TOOL_MEDIA_IMAGES} imágenes por respuesta"));
        }
        if attached.clone().any(|other| other.raster.sha256() == image.raster.sha256()) {
            return Err("La imagen ya está adjunta".to_owned());
        }
        let bytes = attached.map(|other| other.raster.size_bytes() as usize).sum::<usize>();
        if bytes + image.raster.size_bytes() as usize > MAX_TOOL_MEDIA_BYTES {
            return Err("Las imágenes de una respuesta no pueden superar 8 MiB en total".to_owned());
        }
        let Some(images) = self.images.get_mut(question) else {
            return Err("La pregunta ya no está activa".to_owned());
        };
        images.push(image);
        Ok(())
    }

    pub(crate) fn detach(&mut self, question: usize, index: usize) {
        if let Some(images) = self.images.get_mut(question).filter(|images| index < images.len()) {
            images.remove(index);
        }
    }

    /// Every attached image, in the order the answer numbers them.
    pub(crate) fn attached(&self) -> Vec<UserImage> {
        self.images.iter().flatten().map(QuestionImage::user_image).collect()
    }

    fn answered(&self, question: usize) -> bool {
        self.picked.get(question).is_some_and(|picked| !picked.is_empty())
            || self.text.get(question).is_some_and(|text| !text.trim().is_empty())
            || self.images.get(question).is_some_and(|images| !images.is_empty())
    }

    pub(crate) fn any_answer(&self, questions: usize) -> bool {
        (0..questions).any(|question| self.answered(question))
    }
}

/// The tool result: what the user answered, question by question. A question
/// left untouched is reported as skipped, so the model never reads silence
/// as a choice. Attached images are named by their position among the
/// result's images, which `QuestionDraft::attached` gives in the same order.
pub(crate) fn answer_json(questions: &[Question], draft: &QuestionDraft) -> String {
    let mut numbered = 0;
    let answers = questions
        .iter()
        .enumerate()
        .map(|(index, question)| {
            if !draft.answered(index) {
                return json!({
                    "header": question.header,
                    "skipped": true,
                    "note": "The user skipped this question."
                });
            }
            let mut answer = json!({
                "header": question.header,
                "selected": draft.picked[index]
                    .iter()
                    .filter_map(|option| question.options.get(*option))
                    .map(|(label, _)| label.as_str())
                    .collect::<Vec<_>>(),
            });
            let text = draft.text[index].trim();
            if !text.is_empty() {
                answer["other_answer"] = json!(text);
            }
            let images = draft.images.get(index).map_or(0, Vec::len);
            if images > 0 {
                answer["attached_images"] = json!((numbered + 1..=numbered + images).collect::<Vec<_>>());
                numbered += images;
            }
            answer
        })
        .collect::<Vec<_>>();
    let mut answer = json!({"answered_by": "user", "answers": answers});
    if numbered > 0 {
        answer["attached_images_note"] =
            json!("attached_images are positions among the images of this result, in order.");
    }
    answer.to_string()
}

pub(crate) fn pick_action(request_id: &str, question: usize, option: usize) -> String {
    format!("question_pick:{request_id}:{question}:{option}")
}

pub(crate) fn text_id(request_id: &str, question: usize) -> String {
    format!("question_text:{request_id}:{question}")
}

/// The request and question an answer field's id names.
pub(crate) fn text_target(id: &str) -> Option<(&str, usize)> {
    let (request_id, question) = id.strip_prefix("question_text:")?.rsplit_once(':')?;
    Some((request_id, question.parse().ok()?))
}

pub(crate) fn action(request_id: &str, verb: &str) -> String {
    format!("question_{verb}:{request_id}")
}

pub(crate) fn detach_action(request_id: &str, question: usize, index: usize) -> String {
    format!("question_detach:{request_id}:{question}:{index}")
}

/// Radio for one choice, box for several: limn has neither control.
fn marker(multi_select: bool, selected: bool) -> Element {
    let ink = if selected {
        theme::BRAND
    } else {
        theme::FOREGROUND_SUBTLE
    };
    let mark = match (multi_select, selected) {
        (true, true) => Element::icon(Icon::Check)
            .foreground(theme::BRAND)
            .size(Some(10.0), Some(10.0)),
        (false, true) => Element::spacer()
            .size(Some(6.0), Some(6.0))
            .background(theme::BRAND)
            .radius(3.0)
            .centered_x()
            .size(None, Some(6.0))
            .centered_y(),
        _ => Element::spacer(),
    };
    Element::panel(mark)
        .size(Some(14.0), Some(14.0))
        .center_content()
        .border(ink, 1.0)
        .radius(if multi_select { 3.0 } else { 7.0 })
}

/// The marker sits on the first line of the option's copy.
fn lowered(marker: Element) -> Element {
    Element::column(vec![Element::spacer().size(None, Some(2.0)), marker])
}

fn header(title: Element, trailing: Option<Element>) -> Element {
    let mut row = vec![
        Element::panel(
            Element::icon(Icon::Chat)
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some(14.0), Some(14.0)),
        )
        .size(Some(14.0), Some(18.0))
        .center_content(),
        title.flex(1.0),
    ];
    row.extend(trailing);
    Element::row(row).min_size(0.0, 18.0).gap(8.0)
}

fn small(text: impl Into<String>) -> Element {
    Text::new(text, theme::component_palette())
        .role(TextRole::Small)
        .muted()
        .render()
}

/// The pending question, answered from this card.
pub(crate) fn card(permission: &PermissionView, state: &ViewState, _compact: bool) -> Element {
    let questions = parse(&serde_json::from_str(&permission.arguments).unwrap_or(Value::Null));
    let request = permission.request_id.as_str();
    let mut draft = state.question_drafts.get(request).cloned().unwrap_or_default();
    draft.fit(questions.len());
    let page = draft.page;
    let Some(question) = questions.get(page) else {
        return Element::spacer();
    };
    let several = questions.len() > 1;
    let mut content = vec![header(
        Text::new(&question.header, theme::component_palette())
            .render()
            .centered_y(),
        several.then(|| small(format!("{} de {}", page + 1, questions.len())).centered_y()),
    )];
    content.push(Text::new(&question.question, theme::component_palette()).render());
    content.push(
        Element::column(
            question
                .options
                .iter()
                .enumerate()
                .map(|(index, (label, description))| {
                    let selected = draft.picked[page].contains(&index);
                    let mut copy = vec![Text::new(label, theme::component_palette())
                        .ink(if selected {
                            theme::FOREGROUND
                        } else {
                            theme::FOREGROUND_SUBTLE
                        })
                        .render()];
                    if !description.is_empty() {
                        copy.push(small(description));
                    }
                    let row = Element::row(vec![
                        lowered(marker(question.multi_select, selected)),
                        Element::column(copy).gap(1.0).flex(1.0),
                        small((index + 1).to_string()),
                    ])
                    .gap(8.0)
                    .padding(6.0);
                    theme::card_palette()
                        .interactive_surface(Element::clickable(
                            pick_action(request, page, index),
                            row,
                        ))
                        .radius(6.0)
                })
                .collect(),
        )
        .gap(2.0),
    );
    content.push(
        TextArea::new(
            text_id(request, page),
            &draft.text[page],
            "Otra respuesta",
            theme::component_palette(),
        )
        .height(44.0)
        .render(),
    );
    if !draft.images[page].is_empty() {
        // The composer's image cards, in a strip a narrow card can scroll.
        content.push(
            Element::horizontal_scroll(
                format!("question_images:{request}:{page}"),
                Element::row(
                    draft.images[page]
                        .iter()
                        .enumerate()
                        .map(|(index, image)| {
                            Attachment::new(
                                format!("question_image:{request}:{page}:{index}"),
                                &image.label,
                                theme::component_palette(),
                            )
                            .preview(image.raster.clone())
                            .preview_only()
                            .remove_action(detach_action(request, page, index))
                            .width(Attachment::PREVIEW_SIZE)
                            .render()
                        })
                        .collect(),
                )
                .gap(8.0),
            )
            .size(None, Some(Attachment::PREVIEW_SIZE)),
        );
    }
    let ghost = |verb: &str, label: &str| {
        Button::new(action(request, verb), label, theme::component_palette())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .render()
    };
    content.push(
        Element::row(vec![
            ghost("attach", "Adjuntar imagen"),
            ghost("paste", "Pegar imagen"),
            Element::spacer().flex(1.0),
        ])
        .gap(4.0),
    );
    if let Some(error) = &draft.image_error {
        content.push(
            Text::new(error, theme::component_palette())
                .role(TextRole::Small)
                .ink(theme::DESTRUCTIVE)
                .render(),
        );
    }
    let last = page + 1 == questions.len();
    let mut buttons = Vec::new();
    if page > 0 {
        buttons.push(
            Button::new(action(request, "back"), "Atrás", theme::component_palette())
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .render(),
        );
    }
    buttons.push(if last {
        theme::primary(action(request, "send"), "Enviar respuesta")
            .size(ControlSize::Compact)
            .enabled(draft.any_answer(questions.len()))
            .render()
    } else {
        theme::primary(action(request, "next"), "Siguiente")
            .size(ControlSize::Compact)
            .render()
    });
    content.push(
        ButtonGroup::new(buttons)
            .leading(
                Button::new(action(request, "skip"), "Omitir", theme::component_palette())
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Compact)
                    .render(),
            )
            .align_end()
            .render(),
    );
    Card::new(Element::column(content).gap(8.0), theme::card_palette()).render()
}

/// The card while the model is still writing the call: the header and the
/// rows of one question with two options, the least a question has.
pub(crate) fn skeleton(scope: &str, arguments: &Value) -> Element {
    let first = parse(arguments).into_iter().next();
    let bar = |index: usize, fraction: f32| {
        tool_card_skeleton::bar(
            format!("tool-skeleton:{scope}:{index}"),
            fraction,
            tool_card_skeleton::on_card(),
        )
    };
    let title = match first.as_ref().filter(|question| !question.header.is_empty()) {
        Some(question) => Text::new(&question.header, theme::component_palette())
            .render()
            .centered_y(),
        None => Element::row(vec![
            Skeleton::line(
                format!("tool-skeleton:{scope}:title"),
                tool_card_skeleton::on_card(),
                132.0,
            )
            .height(tool_card_skeleton::BAR)
            .radius(tool_card_skeleton::BAR / 2.0)
            .render()
            .centered_y(),
            Element::spacer().flex(1.0),
        ]),
    };
    let mut content = vec![header(title, None)];
    content.push(
        match first.as_ref().filter(|question| !question.question.is_empty()) {
            Some(question) => Text::new(&question.question, theme::component_palette()).render(),
            // An invisible-width figure space would not size the row; a real
            // character does, so the bar sits in a line of body copy.
            None => Element::row(vec![
                Text::new("¿", theme::component_palette()).muted().render(),
                bar(0, 0.72).flex(1.0),
            ])
            .gap(4.0),
        },
    );
    content.push(
        Element::column(
            [0.34, 0.46]
                .into_iter()
                .enumerate()
                .map(|(index, fraction)| {
                    Element::row(vec![
                        lowered(marker(false, false)),
                        Text::new((index + 1).to_string(), theme::component_palette())
                            .muted()
                            .render(),
                        bar(index + 1, fraction).flex(1.0),
                    ])
                    .gap(8.0)
                    .padding(6.0)
                })
                .collect(),
        )
        .gap(2.0),
    );
    Card::new(Element::column(content).gap(8.0), theme::card_palette()).render()
}

/// A finished call: each question's header with what the user answered, and
/// under them the images the answer carried: `media`, the result's own, which
/// open in `owner`'s viewer like any tool image.
pub(crate) fn answered(
    arguments: &Value,
    output: &str,
    media: Option<&TranscriptItem>,
    owner: &str,
) -> Element {
    let questions = parse(arguments);
    let answers = serde_json::from_str::<Value>(output)
        .ok()
        .and_then(|value| value.get("answers").and_then(Value::as_array).cloned());
    let Some(answers) = answers else {
        return Card::new(
            header(
                Text::new("Pregunta omitida", theme::component_palette())
                    .muted()
                    .render()
                    .centered_y(),
                None,
            ),
            theme::card_palette(),
        )
        .render();
    };
    let mut content = vec![header(
        Text::new("Tu respuesta", theme::component_palette())
            .render()
            .centered_y(),
        None,
    )];
    for (index, answer) in answers.iter().enumerate() {
        let heading = answer
            .get("header")
            .and_then(Value::as_str)
            .or_else(|| questions.get(index).map(|question| question.header.as_str()))
            .unwrap_or_default();
        let mut said = answer
            .get("selected")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if let Some(other) = answer.get("other_answer").and_then(Value::as_str) {
            said.push(other.to_owned());
        }
        match answer.get("attached_images").and_then(Value::as_array).map_or(0, Vec::len) {
            0 => {}
            1 => said.push("1 imagen adjunta".to_owned()),
            count => said.push(format!("{count} imágenes adjuntas")),
        }
        let skipped = said.is_empty();
        content.push(
            Element::column(vec![
                small(heading),
                Text::new(
                    if skipped {
                        "Omitida".to_owned()
                    } else {
                        said.join(" · ")
                    },
                    theme::component_palette(),
                )
                .ink(if skipped {
                    theme::FOREGROUND_SUBTLE
                } else {
                    theme::FOREGROUND
                })
                .render(),
            ])
            .gap(1.0),
        );
    }
    if let Some(TranscriptItem::ToolMedia { tool_call_id, images, .. }) = media {
        content.push(
            Element::horizontal_scroll(
                format!("question_answer_images:{tool_call_id}"),
                Element::row(
                    images
                        .iter()
                        .enumerate()
                        .filter_map(|(index, image)| {
                            let raster = image.raster.as_ref()?;
                            Some(
                                Attachment::new(
                                    format!("question_answer_image:{tool_call_id}:{index}"),
                                    format!("Imagen {} de la respuesta", index + 1),
                                    theme::component_palette(),
                                )
                                .preview(raster.clone())
                                .preview_only()
                                .open_action(crate::tool_presentation::image_action(
                                    owner,
                                    tool_call_id,
                                    index,
                                    raster.sha256(),
                                ))
                                .width(Attachment::PREVIEW_SIZE)
                                .render(),
                            )
                        })
                        .collect(),
                )
                .gap(8.0),
            )
            .size(None, Some(Attachment::PREVIEW_SIZE)),
        );
    }
    Card::new(Element::column(content).gap(8.0), theme::card_palette()).render()
}

#[cfg(test)]
#[path = "question_ui_tests.rs"]
mod tests;
