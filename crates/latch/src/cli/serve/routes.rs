//! The protocol-major-2 gateway route and grant registry.
//!
//! Both the Axum router and the paired Noise proxy consume this table. Adding a
//! served route anywhere else would make paired access silently unroutable.

use serde::{Deserialize, Serialize};

pub const DEVICE_GRANT_HEADER: &str = "x-latch-device-grant";

/// Access granted to a paired device and required by a gateway route.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Grant {
    /// Read discovery, sessions, conversations, and output-only terminals.
    Observe,
    /// Also apply structured conversation actions.
    Interact,
    /// Also write terminal bytes and resize panes.
    Control,
}

impl Grant {
    pub(crate) fn permits(self, required: Self) -> bool {
        matches!(
            (self, required),
            (Self::Control, _)
                | (Self::Interact, Self::Interact | Self::Observe)
                | (Self::Observe, Self::Observe)
        )
    }

    pub(crate) const fn as_header_value(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Interact => "interact",
            Self::Control => "control",
        }
    }

    pub(crate) fn from_header_value(value: &str) -> Option<Self> {
        match value {
            "observe" => Some(Self::Observe),
            "interact" => Some(Self::Interact),
            "control" => Some(Self::Control),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteId {
    Capabilities,
    Sessions,
    Session,
    Terminal,
    Conversation,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RouteSpec {
    pub id: RouteId,
    /// Axum registration path.
    pub pattern: &'static str,
    pub method: &'static str,
    pub required_grant: Grant,
}

pub(crate) const ROUTES: &[RouteSpec] = &[
    RouteSpec {
        id: RouteId::Capabilities,
        pattern: "/v2/capabilities",
        method: "GET",
        required_grant: Grant::Observe,
    },
    RouteSpec {
        id: RouteId::Sessions,
        pattern: "/v2/sessions",
        method: "GET",
        required_grant: Grant::Observe,
    },
    RouteSpec {
        id: RouteId::Session,
        pattern: "/v2/sessions/{id}",
        method: "GET",
        required_grant: Grant::Observe,
    },
    RouteSpec {
        id: RouteId::Terminal,
        pattern: "/v2/sessions/{id}/terminal",
        method: "GET",
        required_grant: Grant::Control,
    },
    RouteSpec {
        id: RouteId::Conversation,
        pattern: "/v2/sessions/{id}/conversation",
        method: "GET",
        required_grant: Grant::Observe,
    },
];

/// Resolve one concrete HTTP target against the shared route table.
pub(crate) fn route_for(method: &str, target: &str) -> Option<(RouteSpec, Grant)> {
    let path = target.split('?').next().unwrap_or(target);
    let spec = ROUTES
        .iter()
        .copied()
        .find(|spec| spec.method == method && path_matches(spec.pattern, path))?;
    let required = if spec.id == RouteId::Terminal && read_only_terminal(target) {
        Grant::Observe
    } else {
        spec.required_grant
    };
    Some((spec, required))
}

fn path_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.split('/').filter(|part| !part.is_empty());
    let path = path.split('/').filter(|part| !part.is_empty());
    let mut path = path.peekable();
    for expected in pattern {
        let Some(actual) = path.next() else {
            return false;
        };
        if expected.starts_with('{') {
            if actual.is_empty() {
                return false;
            }
        } else if expected != actual {
            return false;
        }
    }
    path.next().is_none()
}

fn read_only_terminal(target: &str) -> bool {
    target
        .split_once('?')
        .map(|(_, query)| query.split('&').any(|part| part == "mode=read-only"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_resolves_every_v2_route_and_grant() {
        let cases = [
            ("/v2/capabilities", Grant::Observe),
            ("/v2/sessions", Grant::Observe),
            ("/v2/sessions/ses_1", Grant::Observe),
            ("/v2/sessions/ses_1/terminal", Grant::Control),
            (
                "/v2/sessions/ses_1/terminal?cols=80&mode=read-only",
                Grant::Observe,
            ),
            ("/v2/sessions/ses_1/conversation", Grant::Observe),
        ];
        for (target, expected) in cases {
            assert_eq!(
                route_for("GET", target).map(|(_, grant)| grant),
                Some(expected)
            );
        }
        assert!(route_for("POST", "/v2/sessions/ses_1/conversation").is_none());
    }
}
