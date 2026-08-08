#![allow(dead_code)]
#![allow(clippy::wildcard_imports)]

use crate::*;
use serde_json::{Value, json};
use utoipa::openapi::security::{Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi};

#[utoipa::path(get, path="/healthz", responses((status=200, body=HealthResponse)))]
fn health() {}
#[utoipa::path(get, path="/readyz", responses((status=200, body=ReadyResponse)))]
fn ready() {}
#[utoipa::path(get, path="/v1/squads", responses((status=200, body=ListSquadsResponse)))]
fn list_squads() {}
#[utoipa::path(post, path="/v1/squads", request_body=CreateSquadRequest, responses((status=200, body=CreateSquadResponse)))]
fn create_squad() {}
#[utoipa::path(get, path="/v1/squads/{squad}", params(("squad"=String, Path)), responses((status=200, body=GetSquadResponse)))]
fn get_squad() {}
#[utoipa::path(post, path="/v1/squads/{squad}/archive", params(("squad"=String, Path)), security(("sessionCredential"=[])), request_body=ArchiveSquadRequest, responses((status=200, body=ArchiveSquadResponse)))]
fn archive_squad() {}
#[utoipa::path(post, path="/v1/squads/{squad}/join", params(("squad"=String, Path)), request_body=JoinSquadRequest, responses((status=200, body=JoinSquadResponse, headers(("Psst-Session-Credential"=String), ("Cache-Control"=String)))))]
fn join_squad() {}
#[utoipa::path(post, path="/v1/squads/{squad}/resume", params(("squad"=String, Path)), security(("sessionCredential"=[])), request_body=ResumeSquadRequest, responses((status=200, body=ResumeSquadResponse, headers(("Psst-Session-Credential"=String), ("Cache-Control"=String)))))]
fn resume_squad() {}
#[utoipa::path(post, path="/v1/squads/{squad}/leave", params(("squad"=String, Path)), security(("sessionCredential"=[])), request_body=LeaveSquadRequest, responses((status=200, body=LeaveSquadResponse)))]
fn leave_squad() {}
#[utoipa::path(get, path="/v1/squads/{squad}/roster", params(("squad"=String, Path)), responses((status=200, body=RosterResponse)))]
fn roster() {}
#[utoipa::path(post, path="/v1/heartbeat", security(("sessionCredential"=[])), request_body=HeartbeatRequest, responses((status=200, body=HeartbeatResponse)))]
fn heartbeat() {}
#[utoipa::path(post, path="/v1/messages", security(("sessionCredential"=[])), request_body=SendMessageRequest, responses((status=200, body=SendMessageResponse)))]
fn send_message() {}
#[utoipa::path(get, path="/v1/inbox", security(("sessionCredential"=[])), params(InboxQuery), responses((status=200, body=InboxResponse)))]
fn inbox() {}
#[utoipa::path(post, path="/v1/messages/ack", security(("sessionCredential"=[])), request_body=AckMessagesRequest, responses((status=200, body=AckMessagesResponse)))]
fn acknowledge() {}
#[utoipa::path(get, path="/v1/squads/{squad}/transcript", params(("squad"=String, Path), TranscriptQuery), security(("sessionCredential"=[])), responses((status=200, body=TranscriptResponse)))]
fn transcript() {}

struct SecurityAddon;
impl Modify for SecurityAddon {
    fn modify(&self, api: &mut utoipa::openapi::OpenApi) {
        api.components
            .as_mut()
            .expect("components")
            .add_security_scheme(
                "sessionCredential",
                SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
            );
    }
}

#[derive(OpenApi)]
#[openapi(info(title="Psst API", version="v1"), paths(health, ready, list_squads, create_squad, get_squad, archive_squad, join_squad, resume_squad, leave_squad, roster, heartbeat, send_message, inbox, acknowledge, transcript), components(schemas(HealthResponse, ReadyResponse, SquadSummary, CreateSquadRequest, ArchiveSquadRequest, ArchiveSquadResponse, ClientMetadata, JoinSquadRequest, ResumeSquadRequest, SessionResponse, LeaveSquadRequest, LeaveSquadResponse, RosterMember, RosterResponse, HeartbeatRequest, HeartbeatResponse, SendMessageRequest, MessageDto, MessageSequence, SendMessageResponse, InboxResponse, AckMessagesRequest, AckMessagesResponse, TranscriptResponse, ErrorEnvelope, ErrorBody, ApiErrorCode, SquadStateDto, MembershipStateDto, TransportPresenceDto, AgentModeDto, AvailabilityDto, AvailabilitySourceDto, MessagePriorityDto, ApiTimestamp)), modifiers(&SecurityAddon))]
struct PsstApiDoc;

const MUTATION_SCHEMAS: [&str; 9] = [
    "CreateSquadRequest",
    "ArchiveSquadRequest",
    "ClientMetadata",
    "JoinSquadRequest",
    "ResumeSquadRequest",
    "LeaveSquadRequest",
    "HeartbeatRequest",
    "SendMessageRequest",
    "AckMessagesRequest",
];
const ERRORS: [(u16, &str); 8] = [
    (400, "Invalid request"),
    (403, "Not a member"),
    (404, "Not found"),
    (409, "Conflict"),
    (413, "Payload too large"),
    (429, "Rate limited"),
    (500, "Internal error"),
    (503, "Database busy"),
];

fn openapi_value() -> Value {
    let mut value = serde_json::to_value(PsstApiDoc::openapi()).expect("serialize OpenAPI");
    let schemas = value["components"]["schemas"]
        .as_object_mut()
        .expect("schemas");
    for name in MUTATION_SCHEMAS {
        schemas[name]["additionalProperties"] = json!(false);
    }
    schemas["ErrorBody"]["properties"]["details"]["maxProperties"] = json!(16);
    schemas["ErrorBody"]["properties"]["details"]["propertyNames"] = json!({"maxLength":64});
    schemas["ErrorBody"]["properties"]["details"]["additionalProperties"] =
        json!({"type":"string","maxLength":256});
    schemas["ErrorBody"]["properties"]["message"]["minLength"] = json!(1);
    schemas["ErrorBody"]["properties"]["message"]["maxLength"] = json!(512);
    schemas["MessageSequence"]["minimum"] = json!(0);
    schemas["MessageSequence"]["maximum"] = json!(i64::MAX);
    schemas["InboxResponse"]["properties"]["messages"]["maxItems"] = json!(100);
    schemas["TranscriptResponse"]["properties"]["messages"]["maxItems"] = json!(100);

    let id = json!({"type":"string","minLength":5,"maxLength":128,"pattern":"^(sqd|agt|mem|ins|msg)_[a-z0-9-]+$"});
    for (schema, fields) in [
        ("SquadSummary", &["id"][..]),
        (
            "SessionResponse",
            &["agent_id", "membership_id", "instance_id"][..],
        ),
        ("LeaveSquadResponse", &["membership_id"][..]),
        ("RosterMember", &["membership_id"][..]),
        ("MessageDto", &["id"][..]),
    ] {
        for field in fields {
            schemas[schema]["properties"][*field] = id.clone();
        }
    }
    schemas["AckMessagesRequest"]["properties"]["message_ids"]["items"] =
        json!({"type":"string","minLength":5,"maxLength":128,"pattern":"^msg_[a-z0-9-]+$"});
    schemas["AckMessagesResponse"]["properties"]["acknowledged_ids"]["items"] =
        json!({"type":"string","minLength":5,"maxLength":128,"pattern":"^msg_[a-z0-9-]+$"});
    for schema in ["CreateSquadRequest", "SquadSummary"] {
        schemas[schema]["properties"]["name"] = json!({"type":"string","minLength":1,"maxLength":64,"pattern":"^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$"});
        schemas[schema]["properties"]["mission"]["x-maxUtf8Bytes"] = json!(4096);
    }
    schemas["JoinSquadRequest"]["properties"]["name"] = json!({"type":"string","minLength":1,"maxLength":64,"pattern":"^[a-z0-9](?:[a-z0-9_.-]*[a-z0-9])?$"});
    schemas["JoinSquadRequest"]["properties"]["role"]["x-maxUtf8Bytes"] = json!(256);
    schemas["JoinSquadRequest"]["properties"]["mission"]["x-maxUtf8Bytes"] = json!(4096);
    for field in ["sender", "recipient"] {
        schemas["MessageDto"]["properties"][field] = json!({"type":"string","minLength":1,"maxLength":64,"pattern":"^[a-z0-9](?:[a-z0-9_.-]*[a-z0-9])?$"});
    }
    schemas["SendMessageRequest"]["properties"]["recipient"] =
        schemas["MessageDto"]["properties"]["recipient"].clone();
    for schema in ["SendMessageRequest", "MessageDto"] {
        schemas[schema]["properties"]["body"]
            .as_object_mut()
            .unwrap()
            .remove("maxLength");
        schemas[schema]["properties"]["body"]["minLength"] = json!(1);
        schemas[schema]["properties"]["body"]["x-maxUtf8Bytes"] = json!(65536);
        schemas[schema]["properties"]["body"]["description"] =
            json!("Non-empty UTF-8; limited by encoded byte length, not Unicode scalar count.");
        schemas[schema]["properties"]["reply_to"]["maxLength"] = json!(128);
        schemas[schema]["properties"]["correlation_id"]["x-maxUtf8Bytes"] = json!(256);
    }
    schemas["SendMessageRequest"]["properties"]["dedupe_key"]["minLength"] = json!(1);
    schemas["SendMessageRequest"]["properties"]["dedupe_key"]["x-maxUtf8Bytes"] = json!(256);
    schemas["ClientMetadata"]["properties"]["kind"]["minLength"] = json!(1);
    schemas["ClientMetadata"]["properties"]["kind"]["x-maxUtf8Bytes"] = json!(64);
    schemas["ClientMetadata"]["properties"]["hostname"]["x-maxUtf8Bytes"] = json!(255);
    schemas["ClientMetadata"]["properties"]["version"]["x-maxUtf8Bytes"] = json!(64);
    for item in value["paths"].as_object_mut().expect("paths").values_mut() {
        for operation in item.as_object_mut().expect("path item").values_mut() {
            let responses = operation["responses"].as_object_mut().expect("responses");
            for (status, description) in ERRORS {
                responses.insert(status.to_string(), json!({"description":description,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/ErrorEnvelope"}}}}));
            }
        }
    }
    for (path, item) in value["paths"].as_object_mut().expect("paths") {
        if path.contains("{squad}") {
            for operation in item.as_object_mut().expect("path item").values_mut() {
                let parameter = operation["parameters"]
                    .as_array_mut()
                    .expect("parameters")
                    .iter_mut()
                    .find(|parameter| parameter["name"] == "squad")
                    .expect("squad parameter");
                parameter["schema"] = json!({"type":"string","minLength":1,"maxLength":64,"pattern":"^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$"});
            }
        }
    }
    for path in ["/v1/squads/{squad}/join", "/v1/squads/{squad}/resume"] {
        let headers = &mut value["paths"][path]["post"]["responses"]["200"]["headers"];
        headers["Psst-Session-Credential"]["schema"]["maxLength"] = json!(172);
        headers["Psst-Session-Credential"]["schema"]["writeOnly"] = json!(true);
        headers["Cache-Control"]["schema"] = json!({"type":"string","const":"no-store"});
    }
    value
}

#[must_use]
/// Generates the canonical `OpenAPI` document.
///
/// # Panics
/// Panics only if the in-memory, serde-compatible `OpenAPI` value cannot be serialized.
pub fn openapi_document() -> String {
    serde_yaml::to_string(&openapi_value()).expect("serialize OpenAPI")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_document_is_checked_in_and_semantically_complete() {
        assert_eq!(
            include_str!("../../../openapi/psst-v1.yaml"),
            openapi_document()
        );
        let value = openapi_value();
        let paths = value["paths"].as_object().unwrap();
        assert_eq!(
            paths
                .values()
                .map(|p| p.as_object().unwrap().len())
                .sum::<usize>(),
            15
        );
        for (path, item) in paths {
            let operation = item.as_object().unwrap().values().next().unwrap();
            assert_eq!(operation["responses"].as_object().unwrap().len(), 9);
            if path.contains("{squad}") {
                let parameter = operation["parameters"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|parameter| parameter["name"] == "squad")
                    .unwrap();
                assert_eq!(parameter["in"], "path");
                assert_eq!(parameter["required"], true);
                assert_eq!(parameter["schema"]["minLength"], 1);
                assert_eq!(parameter["schema"]["maxLength"], 64);
                assert_eq!(
                    parameter["schema"]["pattern"],
                    "^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$"
                );
            }
        }
        let schemas = &value["components"]["schemas"];
        assert!(
            schemas["SendMessageRequest"]["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "dedupe_key")
        );
        assert_eq!(
            schemas["ErrorBody"]["properties"]["details"]["propertyNames"]["maxLength"],
            64
        );
        assert_eq!(
            value["paths"]["/v1/squads/{squad}/join"]["post"]["responses"]["200"]["headers"]["Cache-Control"]
                ["schema"]["const"],
            "no-store"
        );
        assert_eq!(
            value["paths"]["/v1/messages/ack"]["post"]["security"][0]["sessionCredential"],
            json!([])
        );
        assert!(
            schemas["AckMessagesResponse"]["properties"]
                .get("acknowledged_at")
                .is_none()
        );
    }
    #[test]
    fn secret_material_is_absent_from_body_schemas() {
        let schema = openapi_document().to_ascii_lowercase();
        assert!(!schema.contains("resume_token"));
        assert!(!schema.contains("resume-token"));
        assert!(schema.contains("psst-session-credential"));
    }

    #[test]
    fn every_required_domain_bound_is_present_in_schema() {
        let value = openapi_value();
        let s = &value["components"]["schemas"];
        assert_eq!(s["MessageSequence"]["minimum"], 0);
        assert_eq!(s["MessageSequence"]["maximum"], i64::MAX);
        assert_eq!(
            s["InboxResponse"]["properties"]["messages"]["maxItems"],
            100
        );
        assert_eq!(
            s["TranscriptResponse"]["properties"]["messages"]["maxItems"],
            100
        );
        assert_eq!(s["ErrorBody"]["properties"]["message"]["minLength"], 1);
        assert_eq!(s["ErrorBody"]["properties"]["message"]["maxLength"], 512);
        for (schema, field, bound) in [
            ("CreateSquadRequest", "name", 64),
            ("JoinSquadRequest", "name", 64),
            ("SendMessageRequest", "recipient", 64),
            ("SendMessageRequest", "reply_to", 128),
        ] {
            assert_eq!(
                s[schema]["properties"][field]["maxLength"], bound,
                "{schema}.{field}"
            );
        }
        for (schema, field, bound) in [
            ("CreateSquadRequest", "mission", 4096),
            ("JoinSquadRequest", "role", 256),
            ("JoinSquadRequest", "mission", 4096),
            ("SendMessageRequest", "body", 65536),
            ("SendMessageRequest", "dedupe_key", 256),
            ("SendMessageRequest", "correlation_id", 256),
            ("ClientMetadata", "kind", 64),
            ("ClientMetadata", "hostname", 255),
            ("ClientMetadata", "version", 64),
        ] {
            assert_eq!(
                s[schema]["properties"][field]["x-maxUtf8Bytes"], bound,
                "{schema}.{field}"
            );
        }
        assert!(
            s["SendMessageRequest"]["properties"]["body"]
                .get("maxLength")
                .is_none()
        );
        assert_eq!(
            s["AckMessagesRequest"]["properties"]["message_ids"]["minItems"],
            1
        );
        assert_eq!(
            s["AckMessagesRequest"]["properties"]["message_ids"]["maxItems"],
            100
        );
        for schema in MUTATION_SCHEMAS {
            assert_eq!(s[schema]["additionalProperties"], false, "{schema}");
        }
        assert_eq!(
            s["MessageDto"]["properties"]["sequence"]["$ref"],
            "#/components/schemas/MessageSequence"
        );
    }
}
