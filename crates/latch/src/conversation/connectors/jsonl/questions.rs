//! Question text, choices, and structured questions from an
//! `AskUserQuestion` input, whether the transcript or the bridge carried it.
use super::*;

pub(super) fn bridge_questions(input: &Value) -> Vec<crate::conversation::RequestQuestion> {
    let questions: Vec<_> = input
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|q| {
            Some(crate::conversation::RequestQuestion {
                question: q.get("question")?.as_str()?.to_owned(),
                header: q.get("header").and_then(Value::as_str).map(str::to_owned),
                multi_select: q
                    .get("multi_select")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                options: q
                    .get("options")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|o| {
                        Some(crate::conversation::QuestionOption {
                            label: o.get("label")?.as_str()?.to_owned(),
                            description: o
                                .get("description")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                        })
                    })
                    .collect(),
            })
        })
        .collect();
    questions
}

pub(super) fn claude_question_prompt(input: Option<&Value>) -> String {
    input
        .and_then(|input| input.get("questions"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|question| {
            string_value(question, "question").or_else(|| string_value(question, "header"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn claude_question_choices(input: Option<&Value>) -> Vec<String> {
    input
        .and_then(|input| input.get("questions"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|question| {
            question
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|option| string_value(option, "label"))
        .collect()
}
