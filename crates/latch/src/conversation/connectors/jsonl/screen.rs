//! Reading the agent's terminal screen: which dialog is painted, its
//! numbered decisions, and whether the composer is empty.
use super::*;

impl JsonlConnector {
    pub(super) fn current_screen(&mut self, deadline: Duration) -> Result<String> {
        let snapshot = self.control()?.snapshot(deadline)?;
        if self.id == "cursor" {
            // An old empty composer in scrollback is not an input target.
            return Ok(snapshot.lines.join("\n"));
        }
        Ok(snapshot
            .history
            .into_iter()
            .chain(snapshot.lines)
            .collect::<Vec<_>>()
            .join("\n"))
    }

    pub(super) fn observe_screen(&mut self, screen: &str) -> Vec<ConnectorMutation> {
        let mut mutations = self.follow_painted_request(screen);
        // With other dialogs waiting, the numbered decisions on the screen
        // may be another dialog's: only the painted request reads them.
        let is_painted = !self.has_waiting_requests()
            || self.pending_request.as_ref().is_some_and(|shown| {
                painted_request(screen, self.waiting_requests(shown))
                    .is_some_and(|painted| painted.id == shown.id)
            });
        if let Some(request) = self.pending_request.as_mut() {
            if screen_contains_request(screen, request) {
                if is_painted {
                    request.screen_seen = true;
                    let choices = visible_choices(screen, &request.prompt);
                    if !choices.is_empty() && request.choices != choices {
                        request.choices = choices;
                        mutations.push(request_mutation(request, RequestStatus::Pending));
                    }
                }
            } else if request.screen_seen {
                let id = request.id.clone();
                mutations.extend(self.close_request(&id));
            }
        }
        self.screen_can_send = Some(
            self.pending_request.is_none()
                && !self.tool_running
                && screen.lines().any(|line| is_empty_composer(self.id, line)),
        );
        mutations
    }

    /// The key that picks `choice` in the shown request's dialog. The screen
    /// must prove that dialog is the one painted: with other dialogs waiting,
    /// a key pressed into the wrong one answers another call.
    pub(super) fn terminal_choice_key(
        &self,
        screen: &str,
        choice: &str,
    ) -> std::result::Result<String, String> {
        let request = self
            .pending_request
            .as_ref()
            .ok_or_else(|| "no pending request".to_owned())?;
        if !request
            .choices
            .iter()
            .any(|offered| offered.eq_ignore_ascii_case(choice))
        {
            return Err(
                "the selected decision is not among the choices currently offered by Claude"
                    .to_owned(),
            );
        }
        if !screen_contains_request(screen, request) {
            return Err("the requested Claude prompt is no longer visible".to_owned());
        }
        if self.has_waiting_requests()
            && painted_request(screen, self.waiting_requests(request))
                .is_none_or(|painted| painted.id != request.id)
        {
            return Err(
                "another waiting permission dialog may be the one on the screen; answer at the terminal"
                    .to_owned(),
            );
        }
        visible_choice_key(screen, &request.prompt, choice).ok_or_else(|| {
            "the selected decision is no longer identifiable on the current Claude prompt"
                .to_owned()
        })
    }
}

pub(super) fn is_empty_composer(connector: &str, line: &str) -> bool {
    if connector == "cursor" {
        return super::cursor::is_empty_composer(line);
    }
    let line = line.trim_start();
    let markers: &[char] = if connector == "claude" {
        &['❯']
    } else {
        &['›', '>']
    };
    markers.iter().any(|marker| {
        line.strip_prefix(*marker).is_some_and(|rest| {
            let rest = rest.trim();
            rest.is_empty() || (connector == "codex" && rest == "Ask Codex to do anything")
        })
    })
}

/// Of several waiting requests, the one whose dialog Claude is painting: the
/// one whose prompt is the last on the screen with numbered decisions
/// beneath it. None when no prompt is on the screen, or when the request
/// found shares its prompt with another, so the screen cannot tell them apart.
pub(super) fn painted_request<'a>(
    screen: &str,
    requests: impl Iterator<Item = &'a PendingRequest>,
) -> Option<&'a PendingRequest> {
    let lines: Vec<String> = screen.lines().map(str::to_lowercase).collect();
    let requests: Vec<&PendingRequest> = requests.collect();
    let mut painted: Option<(usize, &PendingRequest)> = None;
    let mut is_ambiguous = false;
    for request in &requests {
        let prompt = request.prompt.to_lowercase();
        if prompt.len() < 4 || visible_choices_with_keys(screen, &request.prompt).is_empty() {
            continue;
        }
        let Some(line) = lines.iter().rposition(|line| line.contains(&prompt)) else {
            continue;
        };
        match painted {
            Some((last, _)) if line < last => {}
            Some((last, _)) if line == last => is_ambiguous = true,
            _ => {
                painted = Some((line, request));
                is_ambiguous = false;
            }
        }
    }
    let (_, request) = painted.filter(|_| !is_ambiguous)?;
    let shared = requests
        .iter()
        .filter(|other| other.prompt.eq_ignore_ascii_case(&request.prompt))
        .count();
    (shared == 1).then_some(request)
}

fn screen_contains_request(screen: &str, request: &PendingRequest) -> bool {
    let screen = screen.to_lowercase();
    let prompt = request.prompt.to_lowercase();
    (prompt.len() >= 4 && screen.contains(&prompt))
        || request
            .choices
            .iter()
            .any(|choice| choice.len() >= 2 && screen.contains(&choice.to_lowercase()))
}

fn visible_choices_with_keys(screen: &str, prompt: &str) -> Vec<(String, String)> {
    let lines: Vec<_> = screen.lines().collect();
    let prompt = prompt.to_lowercase();
    let Some(prompt_line) = lines
        .iter()
        .rposition(|line| prompt.len() >= 4 && line.to_lowercase().contains(&prompt))
    else {
        return Vec::new();
    };
    lines[prompt_line + 1..]
        .iter()
        .copied()
        .filter_map(|line| {
            let line = line
                .trim_start()
                .trim_start_matches(['❯', '>'])
                .trim_start();
            let (number, label) = line.split_once('.')?;
            let number = number.trim();
            let label = label.trim();
            (!label.is_empty()
                && matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"))
            .then(|| (number.to_owned(), label.to_owned()))
        })
        .collect()
}

pub(super) fn visible_choices(screen: &str, prompt: &str) -> Vec<String> {
    visible_choices_with_keys(screen, prompt)
        .into_iter()
        .map(|(_, label)| label)
        .collect()
}

fn visible_choice_key(screen: &str, prompt: &str, choice: &str) -> Option<String> {
    visible_choices_with_keys(screen, prompt)
        .into_iter()
        .find_map(|(key, label)| label.eq_ignore_ascii_case(choice).then_some(key))
}
