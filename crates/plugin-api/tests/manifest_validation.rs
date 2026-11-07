//! Manifest schema v1: acceptance, version negotiation and rejection paths.
use ukk_plugin_api::{
    Capability, ClientPlatform, CommandScope, ContributionSlot, EventKind, ManifestError,
    ManifestErrorCode, OverlayAnchor, PluginId, PluginManifest, TrustLevel, ValidatedManifest,
};

const BASE: &str = r#"
manifest_version = 1
id = "dev.ukk.test.plugin"
name = "Test Plugin"
version = "0.1.0"
api = ">=0.1,<0.2"
trust = "first-party"
entrypoint = "test-helper"
"#;

fn manifest(extra: &str) -> String {
    format!("{BASE}\n{extra}")
}

fn error(text: &str) -> ManifestError {
    match ValidatedManifest::from_toml_str(text) {
        Ok(_) => panic!("manifest unexpectedly validated"),
        Err(error) => error,
    }
}

fn assert_code(text: &str, code: ManifestErrorCode) {
    let error = error(text);
    assert_eq!(error.code(), code, "unexpected error: {error}");
}

#[test]
fn shipped_sample_manifest_is_valid() {
    let text = include_str!("../../../plugins/sample-session-action/plugin.toml");
    let manifest = ValidatedManifest::from_toml_str(text).expect("sample manifest must validate");

    assert_eq!(manifest.id().as_str(), "dev.ukk.sample.session-action");
    assert_eq!(manifest.name(), "Sample Session Action");
    assert_eq!(manifest.version().to_string(), "0.1.0");
    assert_eq!(manifest.api().as_str(), ">=0.1.0,<0.2.0");
    assert_eq!(manifest.trust(), TrustLevel::FirstParty);
    assert!(!manifest.trust().runs_in_process());
    assert_eq!(manifest.entrypoint(), Some("ukk-sample-session-helper"));

    assert!(manifest.required_capabilities().is_empty());
    assert_eq!(manifest.optional_capabilities().len(), 1);
    assert!(manifest
        .declared_capabilities()
        .contains(&Capability::SessionObserve));

    assert_eq!(manifest.commands().len(), 1);
    let command = manifest.command("sample.session.ping").unwrap();
    assert_eq!(command.scope(), CommandScope::Session);
    assert!(command.requires().is_empty());

    assert_eq!(manifest.views().len(), 3);
    let action = manifest.view("sample.session.action").unwrap();
    assert_eq!(action.slot(), ContributionSlot::SessionAction);
    assert_eq!(action.command(), Some("sample.session.ping"));
    assert_eq!(action.platforms(), &[ClientPlatform::Desktop]);

    let tab = manifest.view("sample.session.tab").unwrap();
    assert_eq!(tab.slot(), ContributionSlot::SessionTab);

    let overlay = manifest.view("sample.session.overlay").unwrap();
    assert_eq!(overlay.slot(), ContributionSlot::SessionOverlay);
    let descriptor = overlay.overlay().expect("overlay descriptor");
    assert_eq!(descriptor.anchor(), OverlayAnchor::TopRight);
    assert_eq!(descriptor.max_width(), 320);
    assert_eq!(descriptor.max_height(), 240);

    assert_eq!(manifest.events().len(), 1);
    assert!(manifest.subscribes_to(EventKind::SessionUpdated));
    assert!(!manifest.subscribes_to(EventKind::AgentStatus));

    // Declaring a capability is not a grant: the type exposes no authority.
    let (plugin, grants) = (manifest.id().clone(), 0_u8);
    assert_eq!(plugin.as_str(), "dev.ukk.sample.session-action");
    assert_eq!(grants, 0);
}

#[test]
fn minimal_helper_manifest_is_valid() {
    let manifest = ValidatedManifest::from_toml_str(BASE).unwrap();
    assert_eq!(manifest.commands().len(), 0);
    assert!(manifest.declared_capabilities().is_empty());
    assert_eq!(manifest.trust(), TrustLevel::FirstParty);
}

#[test]
fn schema_parser_and_validator_are_separate_steps() {
    let parsed = PluginManifest::parse_str(BASE).unwrap();
    assert_eq!(parsed.manifest_version(), 1);
    assert_eq!(parsed.id_text(), "dev.ukk.test.plugin");
    let validated = parsed.validate().unwrap();
    assert_eq!(validated.id().as_str(), "dev.ukk.test.plugin");
}

#[test]
fn rejects_unsupported_manifest_version() {
    let text = BASE.replace("manifest_version = 1", "manifest_version = 2");
    let error = error(&text);
    assert_eq!(error.code(), ManifestErrorCode::UnsupportedManifestVersion);
    assert_eq!(error.plugin_id(), Some("dev.ukk.test.plugin"));
}

#[test]
fn rejects_incompatible_api_range() {
    let text = BASE.replace(">=0.1,<0.2", ">=0.2,<0.3");
    assert_code(&text, ManifestErrorCode::IncompatibleApi);

    let text = BASE.replace(">=0.1,<0.2", ">0.1.0,<0.2");
    assert_code(&text, ManifestErrorCode::IncompatibleApi);

    let exact = BASE.replace(">=0.1,<0.2", "=0.1.0");
    assert!(ValidatedManifest::from_toml_str(&exact).is_ok());
}

#[test]
fn rejects_malformed_api_range() {
    let text = BASE.replace(">=0.1,<0.2", "~>0.1");
    assert_code(&text, ManifestErrorCode::InvalidApiRange);
}

#[test]
fn rejects_unknown_and_unimplemented_trust_levels() {
    let text = BASE.replace("trust = \"first-party\"", "trust = \"root\"");
    assert_code(&text, ManifestErrorCode::UnknownTrustLevel);

    let text = BASE.replace("trust = \"first-party\"", "trust = \"sandboxed-wasm\"");
    let error = error(&text);
    assert_eq!(error.code(), ManifestErrorCode::UnknownTrustLevel);
    assert!(error.detail().contains("no WASM runtime"));
}

#[test]
fn rejects_invalid_plugin_ids() {
    for bad in [
        "dev", "Dev.ukk", "dev/ukk", "dev..ukk", "dev.ukk.", "-dev.ukk",
    ] {
        let text = BASE.replace("dev.ukk.test.plugin", bad);
        assert_code(&text, ManifestErrorCode::InvalidId);
    }
    assert!(PluginId::parse("dev.ukk.test").is_ok());
}

#[test]
fn rejects_unknown_capability_names() {
    let text = manifest("[capabilities]\nrequired = [\"screen.everything\"]\n");
    let error = error(&text);
    assert_eq!(error.code(), ManifestErrorCode::UnknownCapability);
    assert!(error.detail().contains("screen.everything"));
}

#[test]
fn network_capability_needs_a_concrete_domain() {
    let text = manifest("[capabilities]\nrequired = [\"network.connect\"]\n");
    assert_code(&text, ManifestErrorCode::UnknownCapability);

    let text = manifest("[capabilities]\nrequired = [\"network.connect(api.example.com)\"]\n");
    let validated = ValidatedManifest::from_toml_str(&text).unwrap();
    assert!(validated
        .required_capabilities()
        .contains(&Capability::NetworkConnect("api.example.com".into())));
}

#[test]
fn rejects_capability_declared_in_both_lists() {
    let text = manifest(
        "[capabilities]\nrequired = [\"session.observe\"]\noptional = [\"session.observe\"]\n",
    );
    assert_code(&text, ManifestErrorCode::DuplicateCapability);

    let text = manifest("[capabilities]\nrequired = [\"session.observe\", \"session.observe\"]\n");
    assert_code(&text, ManifestErrorCode::DuplicateCapability);
}

#[test]
fn command_may_only_require_declared_capabilities() {
    let text = manifest(
        "[[contributions.commands]]\nid = \"test.approve\"\ntitle = \"Approve\"\nscope = \"session\"\nrequires = [\"agent.approve\"]\n",
    );
    assert_code(&text, ManifestErrorCode::UndeclaredCapability);

    let text = manifest(
        "[capabilities]\noptional = [\"agent.approve\"]\n\n[[contributions.commands]]\nid = \"test.approve\"\ntitle = \"Approve\"\nscope = \"session\"\nrequires = [\"agent.approve\"]\n",
    );
    let validated = ValidatedManifest::from_toml_str(&text).unwrap();
    assert!(validated
        .command("test.approve")
        .unwrap()
        .requires()
        .contains(&Capability::AgentApprove));
}

#[test]
fn rejects_unknown_slot_scope_event_kind_and_platform() {
    let text = manifest("[[contributions.views]]\nid = \"test.view\"\nslot = \"session.sidebar\"\ntitle = \"View\"\n");
    assert_code(&text, ManifestErrorCode::UnknownSlot);

    let text = manifest(
        "[[contributions.commands]]\nid = \"test.run\"\ntitle = \"Run\"\nscope = \"machine\"\n",
    );
    assert_code(&text, ManifestErrorCode::UnknownScope);

    let text = manifest(
        "[capabilities]\noptional = [\"session.observe\"]\n\n[[contributions.events]]\nid = \"test.events\"\nkinds = [\"terminal.output\"]\n",
    );
    assert_code(&text, ManifestErrorCode::UnknownEventKind);

    let text = manifest(
        "[[contributions.commands]]\nid = \"test.run\"\ntitle = \"Run\"\nscope = \"session\"\n\n[[contributions.views]]\nid = \"test.view\"\nslot = \"session.tab\"\ntitle = \"View\"\ncommand = \"test.run\"\nplatforms = [\"android\"]\n",
    );
    assert_code(&text, ManifestErrorCode::UnknownPlatform);
}

#[test]
fn rejects_duplicate_contributions() {
    let text = manifest(
        "[[contributions.commands]]\nid = \"test.run\"\ntitle = \"Run\"\nscope = \"session\"\n\n[[contributions.commands]]\nid = \"test.run\"\ntitle = \"Run again\"\nscope = \"session\"\n",
    );
    assert_code(&text, ManifestErrorCode::DuplicateCommand);

    let text = manifest(
        "[[contributions.views]]\nid = \"test.view\"\nslot = \"session.tab\"\ntitle = \"One\"\n\n[[contributions.views]]\nid = \"test.view\"\nslot = \"session.tab\"\ntitle = \"Two\"\n",
    );
    assert_code(&text, ManifestErrorCode::DuplicateView);

    let text = manifest(
        "[capabilities]\noptional = [\"session.observe\"]\n\n[[contributions.events]]\nid = \"test.events\"\nkinds = [\"session.updated\"]\n\n[[contributions.events]]\nid = \"test.events\"\nkinds = [\"agent.status\"]\n",
    );
    assert_code(&text, ManifestErrorCode::DuplicateEventSubscription);

    let text = manifest(
        "[capabilities]\noptional = [\"session.observe\"]\n\n[[contributions.events]]\nid = \"test.events\"\nkinds = [\"session.updated\", \"session.updated\"]\n",
    );
    assert_code(&text, ManifestErrorCode::DuplicateEventKind);
}

#[test]
fn event_subscriptions_require_observe_capability() {
    let text =
        manifest("[[contributions.events]]\nid = \"test.events\"\nkinds = [\"session.updated\"]\n");
    let error = error(&text);
    assert_eq!(error.code(), ManifestErrorCode::UndeclaredCapability);
    assert!(error.detail().contains("session.observe"));
}

#[test]
fn rejects_view_bound_to_undeclared_command() {
    let text = manifest(
        "[[contributions.views]]\nid = \"test.view\"\nslot = \"session.tab\"\ntitle = \"View\"\ncommand = \"test.missing\"\n",
    );
    assert_code(&text, ManifestErrorCode::UnknownCommandReference);
}

#[test]
fn session_slots_must_bind_session_scoped_commands() {
    let text = manifest(
        "[[contributions.commands]]\nid = \"test.global\"\ntitle = \"Global\"\nscope = \"global\"\n\n[[contributions.views]]\nid = \"test.tab\"\nslot = \"session.tab\"\ntitle = \"Tab\"\ncommand = \"test.global\"\n",
    );
    let error = error(&text);
    assert_eq!(error.code(), ManifestErrorCode::CommandScopeMismatch);

    let text = manifest(
        "[[contributions.commands]]\nid = \"test.workspace\"\ntitle = \"Workspace\"\nscope = \"workspace\"\n\n[[contributions.views]]\nid = \"test.action\"\nslot = \"session.action\"\ntitle = \"Action\"\ncommand = \"test.workspace\"\n",
    );
    assert_code(&text, ManifestErrorCode::CommandScopeMismatch);

    // A settings section may legitimately bind a host-wide command.
    let text = manifest(
        "[[contributions.commands]]\nid = \"test.global\"\ntitle = \"Global\"\nscope = \"global\"\n\n[[contributions.views]]\nid = \"test.settings\"\nslot = \"settings.section\"\ntitle = \"Settings\"\ncommand = \"test.global\"\n",
    );
    let validated = ValidatedManifest::from_toml_str(&text).unwrap();
    assert_eq!(validated.views().len(), 1);

    // Session slot with a session command stays valid.
    let text = manifest(
        "[[contributions.commands]]\nid = \"test.session\"\ntitle = \"Session\"\nscope = \"session\"\n\n[[contributions.views]]\nid = \"test.tab\"\nslot = \"session.tab\"\ntitle = \"Tab\"\ncommand = \"test.session\"\n",
    );
    assert!(ValidatedManifest::from_toml_str(&text).is_ok());
}

#[test]
fn overlay_descriptor_rules() {
    let text = manifest("[[contributions.views]]\nid = \"test.overlay\"\nslot = \"session.overlay\"\ntitle = \"Overlay\"\n");
    assert_code(&text, ManifestErrorCode::OverlayMissing);

    let text = manifest(
        "[[contributions.views]]\nid = \"test.tab\"\nslot = \"session.tab\"\ntitle = \"Tab\"\n\n[contributions.views.overlay]\nanchor = \"top-right\"\nmax_width = 320\nmax_height = 240\n",
    );
    assert_code(&text, ManifestErrorCode::OverlayNotAllowed);

    let text = manifest(
        "[[contributions.views]]\nid = \"test.overlay\"\nslot = \"session.overlay\"\ntitle = \"Overlay\"\n\n[contributions.views.overlay]\nanchor = \"top-right\"\nmax_width = 40\nmax_height = 240\n",
    );
    assert_code(&text, ManifestErrorCode::OverlayBounds);

    let text = manifest(
        "[[contributions.views]]\nid = \"test.overlay\"\nslot = \"session.overlay\"\ntitle = \"Overlay\"\n\n[contributions.views.overlay]\nanchor = \"nowhere\"\nmax_width = 320\nmax_height = 240\n",
    );
    assert_code(&text, ManifestErrorCode::OverlayBounds);
}

#[test]
fn entrypoint_rules() {
    // Helper plugins must name one.
    let text = BASE.replace("entrypoint = \"test-helper\"\n", "");
    assert_code(&text, ManifestErrorCode::EntrypointInvalid);

    // Never a path.
    for bad in ["../evil", "sub/helper", "./helper", "helper sh"] {
        let text = BASE.replace(
            "entrypoint = \"test-helper\"",
            &format!("entrypoint = \"{bad}\""),
        );
        assert_code(&text, ManifestErrorCode::EntrypointInvalid);
    }

    // Built-in plugins must not.
    let text = BASE.replace("trust = \"first-party\"", "trust = \"builtin\"");
    assert_code(&text, ManifestErrorCode::EntrypointInvalid);

    let text = BASE
        .replace("trust = \"first-party\"", "trust = \"builtin\"")
        .replace("entrypoint = \"test-helper\"\n", "");
    assert!(ValidatedManifest::from_toml_str(&text).is_ok());
}

#[test]
fn contribution_limits_are_enforced() {
    let mut text = manifest("");
    for index in 0..33 {
        text.push_str(&format!(
            "[[contributions.commands]]\nid = \"test.cmd{index}\"\ntitle = \"Command {index}\"\nscope = \"global\"\n\n"
        ));
    }
    assert_code(&text, ManifestErrorCode::LimitExceeded);

    let text = manifest(
        "[[contributions.commands]]\nid = \"x\"\ntitle = \"Too short id\"\nscope = \"global\"\n",
    );
    assert_code(&text, ManifestErrorCode::InvalidContributionId);

    let long_title = "t".repeat(81);
    let text = manifest(&format!(
        "[[contributions.commands]]\nid = \"test.long\"\ntitle = \"{long_title}\"\nscope = \"global\"\n"
    ));
    assert_code(&text, ManifestErrorCode::InvalidTitle);
}

#[test]
fn unknown_fields_are_rejected_at_every_level() {
    let text = format!("{BASE}\nprivileges = [\"root\"]\n");
    assert_code(&text, ManifestErrorCode::InvalidToml);

    let text = manifest("[[contributions.commands]]\nid = \"test.run\"\ntitle = \"Run\"\nscope = \"session\"\ncapability = \"agent.approve\"\n");
    assert_code(&text, ManifestErrorCode::InvalidToml);

    let text = manifest("[capabilities]\nrequired = []\nsecret = [\"agent.approve\"]\n");
    assert_code(&text, ManifestErrorCode::InvalidToml);
}

#[test]
fn malformed_and_incomplete_documents_are_distinguished() {
    assert_code("manifest_version = ", ManifestErrorCode::InvalidToml);

    let text = BASE.replace("id = \"dev.ukk.test.plugin\"\n", "");
    assert_code(&text, ManifestErrorCode::MissingField);

    let text = manifest("[[contributions.commands]]\nid = \"test.run\"\ntitle = \"Run\"\n");
    assert_code(&text, ManifestErrorCode::MissingField);
}

#[test]
fn rejects_invalid_versions_and_names() {
    let text = BASE.replace("version = \"0.1.0\"", "version = \"0.1.0-beta\"");
    assert_code(&text, ManifestErrorCode::InvalidVersion);

    let text = BASE.replace("version = \"0.1.0\"", "version = \"1\"");
    assert_code(&text, ManifestErrorCode::InvalidVersion);

    let text = BASE.replace("name = \"Test Plugin\"", "name = \"\"");
    assert_code(&text, ManifestErrorCode::InvalidTitle);
}

#[test]
fn parse_path_reports_unreadable_files_without_leaking_contents() {
    let error = PluginManifest::parse_path(std::path::Path::new("/nonexistent/ukk/plugin.toml"))
        .unwrap_err();
    assert_eq!(error.code(), ManifestErrorCode::Unreadable);
    assert!(!error.detail().contains("not found: no such"));
}
