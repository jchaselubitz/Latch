#!/usr/bin/env python3
"""Generate the Swift remote-access contract from the canonical JSON Schemas.

Latch's wire contract is schema-first: `schemas/remote-access/v1/*.schema.json`
and `fixtures/harness/*.v1.json` own it, and each client language generates its
types instead of hand-copying them. This is the Swift target of that rule.

The mobile app is meant to survive being moved to its own repository, so the
schemas it was generated from are vendored into `Contract/schemas/` next to a
`Contract/manifest.json` digest record. That gives two independent checks:

  --check                     the vendored schemas still produce the committed
                              Swift and the committed digests. Works offline
                              and after the folder has moved.
  --check --upstream <root>   additionally, the vendored copies are still
                              byte-identical to the canonical schemas in the
                              Latch repository. This is the drift gate: a new
                              contract version fails here until the app is
                              regenerated against it.

Running with no arguments syncs from upstream and rewrites all three outputs.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path

APP = Path(__file__).resolve().parent.parent
VENDORED = APP / "Contract/schemas"
MANIFEST = APP / "Contract/manifest.json"
SWIFT = APP / "Sources/LatchMobileKit/Generated/LatchContract.swift"

DEFAULT_UPSTREAM = APP.parent.parent

# Canonical source path -> the `$id` it must declare. A schema whose identity
# drifts is a different contract, not a newer copy of this one, so generation
# refuses it rather than silently emitting types for an unknown document.
SOURCES = {
    "schemas/remote-access/v1/gateway-capabilities.schema.json": "https://latch.cooperativ.dev/schemas/remote-access/v1/gateway-capabilities.schema.json",
    "schemas/remote-access/v1/terminal-connection.schema.json": "https://latch.cooperativ.dev/schemas/remote-access/v1/terminal-connection.schema.json",
    "schemas/remote-access/v1/idempotency-key.schema.json": "https://latch.cooperativ.dev/schemas/remote-access/v1/idempotency-key.schema.json",
    "schemas/remote-access/v1/send-request.schema.json": "https://latch.cooperativ.dev/schemas/remote-access/v1/send-request.schema.json",
    "fixtures/harness/harness-event.v1.json": "harness-event.v1.json",
    "fixtures/harness/interaction-capabilities.v1.json": "interaction-capabilities.v1.json",
}

# `gateway-readiness.schema.json` is deliberately absent: it is a private
# handoff between `latch serve` and its desktop supervisor. A phone never reads
# it, so generating a Swift type for it would imply a client surface that the
# gateway does not offer remotely.


class ContractError(SystemExit):
    pass


def load(root: Path, relative: str) -> dict:
    path = root / relative
    if not path.is_file():
        raise ContractError(f"missing contract schema: {path}")
    document = json.loads(path.read_text())
    expected = SOURCES[relative]
    if document.get("$id") != expected:
        raise ContractError(f"{relative} must declare $id {expected}")
    return document


def digest(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()


# ---------------------------------------------------------------------------
# Schema -> Swift
# ---------------------------------------------------------------------------


def camel_to_pascal(name: str) -> str:
    return name[:1].upper() + name[1:]


def snake_to_camel(name: str) -> str:
    head, *rest = name.split("_")
    return head + "".join(part.capitalize() for part in rest)


def kebab_to_camel(name: str) -> str:
    head, *rest = name.split("-")
    return head + "".join(part.capitalize() for part in rest)


def swift_type(schema: object) -> str:
    """Map one JSON Schema property to the Swift type the client decodes into."""
    if schema is True:
        # An intentionally unconstrained payload, such as reduced tool input.
        return "JSONValue"
    if not isinstance(schema, dict):
        raise ContractError(f"unsupported property schema: {schema!r}")
    if "enum" in schema:
        return "String"
    kind = schema.get("type")
    if kind == "string":
        return "String"
    if kind == "boolean":
        return "Bool"
    if kind == "integer":
        return "Int"
    if kind == "array":
        return f"[{swift_type(schema.get('items', True))}]"
    if kind == "object":
        return "JSONValue"
    raise ContractError(f"unsupported property type: {schema!r}")


def boolean_struct(name: str, noun: str, doc: str, properties: dict[str, dict]) -> str:
    """A flat all-boolean object, which both `endpoints` and `features` are."""
    for key, value in properties.items():
        if value.get("type") != "boolean":
            raise ContractError(f"{name}.{key} is no longer a boolean")
    fields = "\n".join(f"    public var {key}: Bool" for key in properties)
    args = ", ".join(f"{key}: Bool = false" for key in properties)
    assigns = "\n".join(f"        self.{key} = {key}" for key in properties)
    cases = "\n".join(f"        case {key}" for key in properties)
    # Every field is decoded permissively: a gateway that omits an optional
    # flag is reporting "not available", which is the same answer as `false`.
    decodes = "\n".join(
        f"        {key} = try container.decodeIfPresent(Bool.self, forKey: .{key}) ?? false"
        for key in properties
    )
    return f"""/// {doc}
public struct {name}: Codable, Equatable, Sendable {{
{fields}

    public init({args}) {{
{assigns}
    }}

    private enum CodingKeys: String, CodingKey {{
{cases}
    }}

    public init(from decoder: Decoder) throws {{
        let container = try decoder.container(keyedBy: CodingKeys.self)
{decodes}
    }}
}}

/// Every {noun} this build knows how to ask about.
public enum {name}Name: String, CaseIterable, Sendable {{
{chr(10).join(f'    case {key}' for key in properties)}
}}

public extension {name} {{
    /// Discovery answer for one {noun}. Anything undiscovered is never assumed.
    func isEnabled(_ name: {name}Name) -> Bool {{
        switch name {{
{chr(10).join(f'        case .{key}: return {key}' for key in properties)}
        }}
    }}
}}
"""


# Properties the event schema declares but never marks required in a `oneOf`
# branch, mapped to the variant that carries them. The schema cannot express
# this itself, so generation asserts the assignment: an unassigned optional
# property fails rather than being silently dropped from the Swift types.
OPTIONAL_EVENT_FIELDS = {"awaiting_input": ["choices"]}


def harness_event_source(schema: dict) -> str:
    envelope = {
        "sessionId": "String",
        "at": "String",
        "harnessVersion": "String",
        "connectorEpoch": "Int",
    }
    base_required = [key for key in schema["required"] if key != "type"]
    if base_required != list(envelope):
        raise ContractError(f"unexpected harness event envelope: {base_required}")

    properties = schema["properties"]
    declared = list(properties["type"]["enum"])
    enums: dict[str, list[str]] = {}
    variants: list[tuple[str, list[tuple[str, str, bool]]]] = []

    for branch in schema["oneOf"]:
        name = branch["properties"]["type"]["const"]
        fields: list[tuple[str, str, bool]] = []
        required = list(branch.get("required", []))
        optional = OPTIONAL_EVENT_FIELDS.get(name, [])
        for field in required + optional:
            if field not in properties:
                raise ContractError(f"{name} references undeclared property {field}")
            spec = properties[field]
            if isinstance(spec, dict) and "enum" in spec:
                # A closed value set becomes its own Swift enum, named for the
                # variant that owns it: awaiting_input.kind -> AwaitingInputKind.
                kind = camel_to_pascal(snake_to_camel(name)) + camel_to_pascal(field)
                enums[kind] = (list(spec["enum"]), f"{name}.{field}")
            else:
                kind = swift_type(spec)
            fields.append((field, kind, field in required))
        variants.append((name, fields))

    if [name for name, _ in variants] != declared:
        raise ContractError("harness event oneOf does not match the type enum")

    assigned = {
        field
        for _, fields in variants
        for field, _, _ in fields
    }
    unassigned = [
        field
        for field in properties
        if field != "type" and field not in envelope and field not in assigned
    ]
    if unassigned:
        raise ContractError(
            "harness event properties belong to no variant: "
            + ", ".join(unassigned)
            + "\nadd them to OPTIONAL_EVENT_FIELDS in this generator"
        )

    enum_sources = "\n".join(
        f"""/// Closed value set for `{wire}` on the harness event contract.
public enum {name}: String, Codable, Sendable, CaseIterable {{
{chr(10).join(f'    case {snake_to_camel(value)} = "{value}"' for value in values)}
}}
"""
        for name, (values, wire) in enums.items()
    )

    cases = []
    for name, fields in variants:
        label = snake_to_camel(name)
        if fields:
            payload = ", ".join(
                f"{field}: {kind}"
                + ("" if required and kind not in enums else "?")
                for field, kind, required in fields
            )
            cases.append(f"    case {label}({payload})")
        else:
            cases.append(f"    case {label}")
    case_lines = "\n".join(cases)

    decode_arms = []
    for name, fields in variants:
        label = snake_to_camel(name)
        reads = []
        for field, kind, required in fields:
            if kind in enums:
                # An unrecognized member of a closed set is additive, not
                # malformed: it decodes to nil so the UI can degrade instead of
                # discarding the whole event.
                reads.append(
                    f"            let {field} = try container"
                    f".decodeIfPresent(String.self, forKey: .{field})"
                    f".flatMap({kind}.init(rawValue:))"
                )
            elif required:
                reads.append(
                    f"            let {field} = try container"
                    f".decode({kind}.self, forKey: .{field})"
                )
            else:
                reads.append(
                    f"            let {field} = try container"
                    f".decodeIfPresent({kind}.self, forKey: .{field})"
                )
        call = ", ".join(f"{field}: {field}" for field, _, _ in fields)
        body = "\n".join(reads)
        arm = f'        case "{name}":'
        if body:
            arm += f"\n{body}"
        arm += f"\n            payload = .{label}({call})" if fields else f"\n            payload = .{label}"
        decode_arms.append(arm)
    decode_body = "\n".join(decode_arms)

    coding_keys = sorted({field for _, fields in variants for field, _, _ in fields})
    keys = "\n".join(f"        case {key}" for key in coding_keys)

    describe = "\n".join(
        f'        case .{snake_to_camel(name)}: return "{name}"' for name, _ in variants
    )

    return f"""{enum_sources}
/// Variant-specific harness event data.
public enum HarnessEventPayload: Equatable, Sendable {{
{case_lines}
    /// A type this build does not know. Additive event types are contract-legal,
    /// so an unknown one is retained and shown as unrecognized rather than
    /// dropped or treated as a decode failure.
    case unknown(type: String)

    /// The wire `type` value, including for an unknown additive variant.
    public var wireType: String {{
        switch self {{
{describe}
        case .unknown(let type): return type
        }}
    }}
}}

/// One normalized harness observation.
public struct HarnessEvent: Decodable, Equatable, Sendable {{
    public let sessionId: String
    public let at: String
    public let harnessVersion: String
    public let connectorEpoch: Int
    public let payload: HarnessEventPayload

    public init(
        sessionId: String,
        at: String,
        harnessVersion: String,
        connectorEpoch: Int,
        payload: HarnessEventPayload
    ) {{
        self.sessionId = sessionId
        self.at = at
        self.harnessVersion = harnessVersion
        self.connectorEpoch = connectorEpoch
        self.payload = payload
    }}

    private enum CodingKeys: String, CodingKey {{
        case type
        case sessionId
        case at
        case harnessVersion
        case connectorEpoch
{keys}
    }}

    public init(from decoder: Decoder) throws {{
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionId = try container.decode(String.self, forKey: .sessionId)
        at = try container.decode(String.self, forKey: .at)
        harnessVersion = try container.decode(String.self, forKey: .harnessVersion)
        connectorEpoch = try container.decode(Int.self, forKey: .connectorEpoch)
        let type = try container.decode(String.self, forKey: .type)
        switch type {{
{decode_body}
        default:
            payload = .unknown(type: type)
            return
        }}
    }}
}}
"""


def interaction_source(schema: dict) -> str:
    required = schema["required"]
    expected = ["sendMessage", "sendKeys", "resolve", "canSend"]
    if required != expected:
        raise ContractError(f"unexpected interaction capabilities: {required}")
    can_send = schema["properties"]["canSend"]
    if can_send["required"] != ["ok"]:
        raise ContractError("canSend must require only ok")
    return """/// Whether input is currently safe for one session.
public struct CanSend: Codable, Equatable, Sendable {
    public var ok: Bool
    public var reason: String?

    public init(ok: Bool, reason: String? = nil) {
        self.ok = ok
        self.reason = reason
    }
}

/// Operations a client may safely offer for one session.
public struct InteractionCapabilities: Codable, Equatable, Sendable {
    public var sendMessage: Bool
    public var sendKeys: Bool
    public var resolve: Bool
    public var canSend: CanSend

    public init(sendMessage: Bool, sendKeys: Bool, resolve: Bool, canSend: CanSend) {
        self.sendMessage = sendMessage
        self.sendKeys = sendKeys
        self.resolve = resolve
        self.canSend = canSend
    }

    private enum CodingKeys: String, CodingKey {
        case sendMessage
        case sendKeys
        case resolve
        case canSend
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sendMessage = try container.decodeIfPresent(Bool.self, forKey: .sendMessage) ?? false
        sendKeys = try container.decodeIfPresent(Bool.self, forKey: .sendKeys) ?? false
        resolve = try container.decodeIfPresent(Bool.self, forKey: .resolve) ?? false
        canSend = try container.decodeIfPresent(CanSend.self, forKey: .canSend)
            ?? CanSend(ok: false, reason: "the gateway did not report canSend")
    }
}
"""


def terminal_mode_source(schema: dict) -> str:
    modes = schema["properties"]["mode"]["enum"]
    cases = "\n".join(
        f'    case {kebab_to_camel(mode)} = "{mode}"' for mode in modes
    )
    return f"""/// Access mode for one terminal WebSocket.
public enum TerminalAccessMode: String, Codable, Sendable {{
{cases}
}}
"""


def swift_source(schemas: dict[str, dict]) -> str:
    capabilities = schemas["schemas/remote-access/v1/gateway-capabilities.schema.json"]
    protocol_version = capabilities["properties"]["protocolVersion"]["const"]
    endpoints = capabilities["properties"]["endpoints"]["properties"]
    features = capabilities["properties"]["features"]["properties"]
    terminal = schemas["schemas/remote-access/v1/terminal-connection.schema.json"]
    idempotency = schemas["schemas/remote-access/v1/idempotency-key.schema.json"]
    send = schemas["schemas/remote-access/v1/send-request.schema.json"]
    events = schemas["fixtures/harness/harness-event.v1.json"]
    interaction = schemas["fixtures/harness/interaction-capabilities.v1.json"]

    send_operations = [branch["required"][0] for branch in send["oneOf"]]
    if idempotency.get("type") != "string":
        raise ContractError("the idempotency key is no longer a string header")
    key_max = idempotency["maxLength"]
    key_pattern = idempotency["pattern"]

    parts = [
        f"""// Generated by Tools/generate-contract.py from Contract/schemas; do not edit by hand.
//
// Run `Tools/generate-contract.py` after the canonical Latch schemas change,
// and `Tools/generate-contract.py --check --upstream <latch-repo>` in CI to
// fail while this app still speaks an older contract.

import Foundation

/// Constants the wire contract fixes for this schema major.
public enum LatchContract {{
    /// Latch gateway protocol major this build understands. A gateway that
    /// reports any other value is unsupported, never guessed at.
    public static let protocolVersion = {protocol_version}
    /// Header carrying the retry-safe submission key, as named by
    /// `idempotency-key.schema.json`.
    public static let idempotencyKeyHeader = "Idempotency-Key"
    /// Longest key the gateway accepts.
    public static let idempotencyKeyMaxLength = {key_max}
    /// Characters a key may contain.
    public static let idempotencyKeyPattern = "{key_pattern}"
    /// Send operations the contract defines, in schema order.
    public static let sendOperations = [{", ".join(f'"{name}"' for name in send_operations)}]
    /// Harness event types this build decodes natively.
    public static let harnessEventTypes = [{", ".join(f'"{name}"' for name in events["properties"]["type"]["enum"])}]
}}
""",
        terminal_mode_source(terminal),
        boolean_struct(
            "GatewayEndpoints",
            "/v1 endpoint",
            "Which /v1 endpoints this gateway serves. Discovery is mandatory: a\n/// client may use an endpoint only when the map reports it as true, and must\n/// never probe an endpoint and infer support from the error.",
            endpoints,
        ),
        boolean_struct(
            "GatewayFeatures",
            "optional v1 feature",
            "Optional v1 gateway features that require discovery before use.",
            features,
        ),
        """/// The mandatory `GET /v1/capabilities` discovery document.
public struct GatewayCapabilities: Decodable, Equatable, Sendable {
    public var protocolVersion: Int
    public var productVersion: String
    public var endpoints: GatewayEndpoints
    public var features: GatewayFeatures
    /// Opaque per-process value. A change means the gateway restarted and its
    /// in-memory idempotency window was lost.
    public var gatewayInstanceId: String

    public init(
        protocolVersion: Int,
        productVersion: String,
        endpoints: GatewayEndpoints,
        features: GatewayFeatures,
        gatewayInstanceId: String
    ) {
        self.protocolVersion = protocolVersion
        self.productVersion = productVersion
        self.endpoints = endpoints
        self.features = features
        self.gatewayInstanceId = gatewayInstanceId
    }

    private enum CodingKeys: String, CodingKey {
        case protocolVersion
        case productVersion
        case endpoints
        case features
        case gatewayInstanceId
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        protocolVersion = try container.decode(Int.self, forKey: .protocolVersion)
        productVersion = try container.decodeIfPresent(String.self, forKey: .productVersion) ?? ""
        endpoints = try container.decodeIfPresent(GatewayEndpoints.self, forKey: .endpoints)
            ?? GatewayEndpoints()
        features = try container.decodeIfPresent(GatewayFeatures.self, forKey: .features)
            ?? GatewayFeatures()
        gatewayInstanceId = try container
            .decodeIfPresent(String.self, forKey: .gatewayInstanceId) ?? ""
    }
}
""",
        interaction_source(interaction),
        harness_event_source(events),
    ]
    return "\n".join(parts)


# ---------------------------------------------------------------------------
# Outputs
# ---------------------------------------------------------------------------


def outputs(root: Path) -> tuple[dict[Path, str], dict[str, dict]]:
    schemas: dict[str, dict] = {}
    files: dict[Path, str] = {}
    entries = []
    for relative in SOURCES:
        text = (root / relative).read_text()
        schemas[relative] = load(root, relative)
        files[VENDORED / Path(relative).name] = text
        entries.append({"source": relative, "sha256": digest(text)})
    source = swift_source(schemas)
    files[SWIFT] = source
    files[MANIFEST] = json.dumps(
        {
            "protocolVersion": schemas[
                "schemas/remote-access/v1/gateway-capabilities.schema.json"
            ]["properties"]["protocolVersion"]["const"],
            "generator": "Tools/generate-contract.py",
            "generated": {"path": str(SWIFT.relative_to(APP)), "sha256": digest(source)},
            "schemas": entries,
        },
        indent=2,
    ) + "\n"
    return files, schemas


def vendored_root() -> Path:
    """A root view of the vendored copies, so generation can run offline.

    The vendored files are stored flat by basename; this maps the canonical
    relative paths back onto them without copying the upstream tree layout.
    """

    class Flat:
        def __truediv__(self, relative: str | Path) -> Path:
            return VENDORED / Path(relative).name

    return Flat()  # type: ignore[return-value]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify the committed outputs instead of rewriting them",
    )
    parser.add_argument(
        "--upstream",
        type=Path,
        help="Latch repository root holding the canonical schemas",
    )
    args = parser.parse_args()

    if args.check:
        files, _ = outputs(vendored_root())
        stale = [
            path
            for path, text in files.items()
            if not path.is_file() or path.read_text() != text
        ]
        if stale:
            names = ", ".join(str(path.relative_to(APP)) for path in stale)
            raise ContractError(f"generated Swift contract is stale: {names}")
        if args.upstream:
            drifted = [
                relative
                for relative in SOURCES
                if (args.upstream / relative).read_text()
                != (VENDORED / Path(relative).name).read_text()
            ]
            if drifted:
                raise ContractError(
                    "the vendored contract no longer matches upstream: "
                    + ", ".join(drifted)
                    + "\nrun Tools/generate-contract.py to adopt the new version"
                )
        print("contract is current")
        return

    root = args.upstream or DEFAULT_UPSTREAM
    if not (root / "schemas/remote-access/v1").is_dir():
        raise ContractError(
            f"{root} does not hold schemas/remote-access/v1; pass --upstream <latch-repo>"
        )
    files, _ = outputs(root)
    for path, text in files.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    print(f"wrote {len(files)} contract files from {root}")


if __name__ == "__main__":
    try:
        main()
    except ContractError as error:
        print(error, file=sys.stderr)
        raise SystemExit(1) from None
