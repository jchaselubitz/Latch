//! The pending-request queue: one request is shown to clients at a time,
//! with bridge-announced dialogs waiting behind it in the order raised.
use super::*;

impl JsonlConnector {
    pub(super) fn has_waiting_requests(&self) -> bool {
        !self.queued_requests.is_empty() || !self.displaced_requests.is_empty()
    }

    /// The shown request, then every request waiting behind it.
    pub(super) fn waiting_requests<'a>(
        &'a self,
        shown: &'a PendingRequest,
    ) -> impl Iterator<Item = &'a PendingRequest> {
        std::iter::once(shown)
            .chain(&self.queued_requests)
            .chain(&self.displaced_requests)
    }

    /// Shows a request the bridge announced under its call id. One request
    /// is shown at a time: while another bridge request is shown, this one
    /// waits behind it in the order the engine raised them, because a
    /// parallel batch raises every dialog at once and Claude paints the
    /// oldest.
    pub(super) fn announce_bridge_request(
        &mut self,
        request: PendingRequest,
    ) -> Vec<ConnectorMutation> {
        if let Some(waiting) = self
            .queued_requests
            .iter_mut()
            .find(|waiting| waiting.id == request.id)
        {
            *waiting = request;
            return Vec::new();
        }
        if self
            .displaced_requests
            .iter()
            .any(|waiting| waiting.id == request.id)
        {
            return Vec::new();
        }
        match self.pending_request.take() {
            Some(shown) if shown.bridge_call && shown.id != request.id => {
                self.pending_request = Some(shown);
                let at = self
                    .queued_requests
                    .iter()
                    .position(|waiting| announced_before(&request, waiting))
                    .unwrap_or(self.queued_requests.len());
                self.queued_requests.insert(at, request);
                Vec::new()
            }
            previous => {
                let mut mutations = Vec::new();
                // The hook may have announced the same dialog first, under a
                // synthetic id; the real one replaces it.
                if let Some(previous) = previous.filter(|previous| previous.id != request.id) {
                    mutations.push(request_mutation(&previous, RequestStatus::Dismissed));
                }
                self.pending_request = Some(request.clone());
                mutations.push(request_mutation(&request, RequestStatus::Pending));
                mutations
            }
        }
    }

    /// Closes the request with this id wherever it waits. Closing the shown
    /// request surfaces the next one.
    pub(super) fn close_request(&mut self, id: &str) -> Vec<ConnectorMutation> {
        if self
            .pending_request
            .as_ref()
            .is_some_and(|shown| shown.id == id)
        {
            let closed = self.pending_request.take().expect("request was present");
            let mut mutations = vec![request_mutation(&closed, RequestStatus::Dismissed)];
            mutations.extend(self.surface_next_request());
            return mutations;
        }
        if let Some(index) = self
            .displaced_requests
            .iter()
            .position(|waiting| waiting.id == id)
        {
            let closed = self.displaced_requests.remove(index);
            return vec![request_mutation(&closed, RequestStatus::Dismissed)];
        }
        // Never shown, so clients hold nothing to dismiss.
        self.queued_requests.retain(|waiting| waiting.id != id);
        Vec::new()
    }

    /// Shows the request next in line once the shown one has closed: the
    /// last one the screen displaced, which clients still hold as their
    /// newest pending request, else the oldest the bridge announced.
    pub(super) fn surface_next_request(&mut self) -> Vec<ConnectorMutation> {
        if self.bridge_version.is_none() {
            // Only the bridge says when a waiting request closes.
            return self.drop_waiting_requests();
        }
        if let Some(request) = self.displaced_requests.pop() {
            self.pending_request = Some(request);
            return Vec::new();
        }
        if self.queued_requests.is_empty() {
            return Vec::new();
        }
        let request = self.queued_requests.remove(0);
        self.pending_request = Some(request.clone());
        vec![request_mutation(&request, RequestStatus::Pending)]
    }

    /// Forgets the requests waiting behind the shown one; they remain the
    /// terminal's to answer.
    pub(super) fn drop_waiting_requests(&mut self) -> Vec<ConnectorMutation> {
        self.queued_requests.clear();
        self.displaced_requests
            .drain(..)
            .map(|request| request_mutation(&request, RequestStatus::Dismissed))
            .collect()
    }

    /// Shows the waiting request whose dialog Claude is painting when that
    /// is not the shown one. Only a request clients have not seen yet can
    /// take its place: the Hub presents the newest pending item, so one they
    /// already hold could not be brought back in front.
    pub(super) fn follow_painted_request(&mut self, screen: &str) -> Vec<ConnectorMutation> {
        if self.queued_requests.is_empty() {
            return Vec::new();
        }
        let Some(shown) = self
            .pending_request
            .as_ref()
            .filter(|shown| shown.bridge_call)
        else {
            return Vec::new();
        };
        let Some(painted) =
            painted_request(screen, self.waiting_requests(shown)).map(|painted| painted.id.clone())
        else {
            return Vec::new();
        };
        let Some(index) = self
            .queued_requests
            .iter()
            .position(|waiting| waiting.id == painted)
        else {
            return Vec::new();
        };
        let mut request = self.queued_requests.remove(index);
        request.screen_seen = true;
        let choices = visible_choices(screen, &request.prompt);
        if !choices.is_empty() {
            request.choices = choices;
        }
        let displaced = self
            .pending_request
            .replace(request.clone())
            .expect("a request was shown");
        self.displaced_requests.push(displaced);
        vec![request_mutation(&request, RequestStatus::Pending)]
    }
}

pub(super) fn request_mutation(
    request: &PendingRequest,
    status: RequestStatus,
) -> ConnectorMutation {
    upsert(
        &format!("request:{}", request.id),
        "1970-01-01T00:00:00Z".to_owned(),
        ConversationItemKind::Request {
            request_id: request.id.clone(),
            request_type: request.request_type.clone(),
            prompt: request.prompt.clone(),
            choices: request.choices.clone(),
            questions: request.questions.clone(),
            status,
        },
    )
}

/// Whether the bridge announced `request` before `other`. Bridge records
/// carry millisecond timestamps taken as each dialog was raised; they may
/// reach the sidecar out of order, since each is its own run of `latch`.
fn announced_before(request: &PendingRequest, other: &PendingRequest) -> bool {
    match (
        request.announced_at.as_deref(),
        other.announced_at.as_deref(),
    ) {
        (Some(request), Some(other)) if request.len() > 20 && other.len() > 20 => request < other,
        _ => false,
    }
}
