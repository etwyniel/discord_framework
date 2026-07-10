use anyhow::{Context, bail};
use serde_derive::{Deserialize, Serialize};

#[derive(Deserialize, Debug)]
pub struct Form {
    #[serde(rename = "formId")]
    pub id: String,
    pub info: Info,
    pub items: Vec<Item>,
    #[serde(rename = "responderUri")]
    pub uri: String,
    #[serde(rename = "linkedSheetId")]
    pub linked_sheet_id: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct Info {
    pub title: Option<String>,
    pub description: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct Item {
    #[serde(rename = "itemId")]
    pub id: String,
    pub title: Option<String>,
    pub description: Option<String>,

    #[serde(rename = "questionItem")]
    pub question: Option<QuestionItem>,
    #[serde(rename = "questionGroupItem")]
    pub question_group: Option<QuestionGroupItem>,
    #[serde(rename = "pageBreakItem")]
    pub page_break: Option<PageBreakItem>,
    #[serde(rename = "textItem")]
    pub text: Option<TextItem>,
    #[serde(rename = "imageItem")]
    pub image: Option<ImageItem>,
    #[serde(rename = "videoItem")]
    pub video: Option<VideoItem>,
}

#[derive(Deserialize, Debug)]
pub struct QuestionItem {
    pub question: Question,
}

#[derive(Deserialize, Debug)]
pub struct QuestionGroupItem {
    pub questions: Vec<Question>,
}

#[derive(Deserialize, Debug)]
pub struct PageBreakItem {}

#[derive(Deserialize, Debug)]
pub struct TextItem {}

#[derive(Deserialize, Debug)]
pub struct ImageItem {}

#[derive(Deserialize, Debug)]
pub struct VideoItem {}

#[derive(Deserialize, Debug)]
pub struct Question {
    #[serde(rename = "questionId")]
    pub id: String,
    #[serde(default)]
    pub required: bool,

    #[serde(rename = "choiceQuestion")]
    pub choice: Option<ChoiceQuestion>,
    #[serde(rename = "textQuestion")]
    pub text: Option<TextQuestion>,
    #[serde(rename = "scaleQuestion")]
    pub scale: Option<ScaleQuestion>,
    #[serde(rename = "dateQuestion")]
    pub date: Option<DateQuestion>,
    #[serde(rename = "timeQuestion")]
    pub time: Option<TimeQuestion>,
    #[serde(rename = "fileUploadQuestion")]
    pub file_upload: Option<FileUploadQuestion>,
    #[serde(rename = "rowQuestion")]
    pub row: Option<RowQuestion>,
}

#[derive(Deserialize, Debug, PartialEq, Eq)]
pub enum ChoiceType {
    #[serde(rename = "RADIO")]
    Radio,
    #[serde(rename = "CHECKBOX")]
    Checkbox,
    #[serde(rename = "DROP_DOWN")]
    DropDown,
}

#[derive(Deserialize, Debug)]
pub struct ChoiceQuestion {
    #[serde(rename = "type")]
    pub ty: ChoiceType,
    pub options: Vec<ChoiceOption>,
}

#[derive(Deserialize, Debug)]
pub struct ChoiceOption {
    #[serde(default)]
    pub value: String,
    #[serde(rename = "isOther", default)]
    pub is_other: bool,
}

#[derive(Deserialize, Debug)]
pub struct TextQuestion {}

#[derive(Deserialize, Debug)]
pub struct ScaleQuestion {
    pub low: i64,
    pub high: i64,
}

#[derive(Deserialize, Debug)]
pub struct DateQuestion {}

#[derive(Deserialize, Debug)]
pub struct TimeQuestion {}

#[derive(Deserialize, Debug)]
pub struct FileUploadQuestion {}

#[derive(Deserialize, Debug)]
pub struct RowQuestion {}

#[derive(Deserialize, Serialize, Debug)]
pub struct SimpleForm {
    pub id: String,
    pub title: String,
    pub questions: Vec<SimpleQuestion>,
    pub responder_uri: String,
    pub sheet_id: Option<String>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct SimpleQuestion {
    #[serde(default)]
    pub id: String,
    pub required: bool,
    pub title: String,
    pub ty: QuestionType,
}

#[derive(Deserialize, Serialize, Debug)]
pub enum QuestionType {
    Text,
    Choice(Vec<String>),
}

impl TryFrom<&Item> for Option<SimpleQuestion> {
    type Error = anyhow::Error;

    fn try_from(value: &Item) -> Result<Self, Self::Error> {
        let question = match &value.question {
            Some(q) => &q.question,
            _ => return Ok(None),
        };
        let title = match value.title.as_deref() {
            Some(title) => title.to_string(),
            None => bail!("Question is missing a title"),
        };
        let required = question.required;
        let ty = if question.text.is_some() {
            QuestionType::Text
        } else if let Some(choice) = question.choice.as_ref() {
            if choice.ty == ChoiceType::Checkbox {
                bail!("Checkboxes are not supported");
            }
            if choice.options.iter().any(|opt| opt.is_other) {
                bail!("'Other' field is not supported");
            }
            let values = choice.options.iter().map(|opt| opt.value.clone()).collect();
            QuestionType::Choice(values)
        } else {
            bail!("Can only handle text or choice questions");
        };
        Ok(Some(SimpleQuestion {
            id: question.id.clone(),
            required,
            title,
            ty,
        }))
    }
}

impl TryFrom<Form> for SimpleForm {
    type Error = anyhow::Error;

    fn try_from(value: Form) -> Result<Self, Self::Error> {
        let id = value.id.clone();
        let title = value
            .info
            .title
            .as_ref()
            .context("Form is missing a title")?
            .clone();
        let questions = value
            .items
            .iter()
            .flat_map(|item| item.try_into().transpose())
            .collect::<anyhow::Result<Vec<_>>>()?;
        let responder_uri = value.uri.clone();
        let sheet_id = value
            .linked_sheet_id
            .as_ref()
            // .ok_or_else(|| anyhow!("No linked spreadsheet"))?
            .cloned();
        Ok(SimpleForm {
            id,
            title,
            questions,
            responder_uri,
            sheet_id,
        })
    }
}
